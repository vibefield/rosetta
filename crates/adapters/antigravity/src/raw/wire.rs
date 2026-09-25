//! Protocol-buffers wire format reader — the bytes-in/fields-out half of the
//! conversation-store decoder.
//!
//! Antigravity's `conversations/<id>.db` blob columns are protobuf messages
//! written by the Go binary. This module knows nothing about any message: it
//! frames one buffer into `(field number, wire value)` pairs and lets the
//! typed decoders in [`super::cortex`] pick the numbers the canonical ground
//! truth declares. Unknown numbers are the caller's drift, not an error;
//! only a buffer that cannot be framed (truncated, oversized varint, group
//! wire types) is malformed.

use std::fmt;

/// One framed field. Length-delimited values borrow the buffer so a nested
/// message costs no copy until a typed decoder asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field<'a> {
    pub number: u32,
    pub value: WireValue<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireValue<'a> {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
}

impl<'a> WireValue<'a> {
    /// A varint scalar (int32/64, uint32/64, bool, enum). Fixed encodings
    /// are accepted too: a writer may legally choose them for the same
    /// numeric field type.
    pub fn as_u64(self) -> Option<u64> {
        match self {
            WireValue::Varint(value) | WireValue::Fixed64(value) => Some(value),
            WireValue::Fixed32(value) => Some(u64::from(value)),
            WireValue::Bytes(_) => None,
        }
    }

    pub fn as_bytes(self) -> Option<&'a [u8]> {
        match self {
            WireValue::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// A `string` field. Protobuf requires UTF-8; anything else is treated as
    /// absent rather than lossily transcoded.
    pub fn as_str(self) -> Option<&'a str> {
        std::str::from_utf8(self.as_bytes()?).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// The buffer ended inside a tag, a varint, or a declared length.
    Truncated { offset: usize },
    /// A varint longer than ten bytes, or a field number of zero.
    InvalidVarint { offset: usize },
    /// Wire types 3/4 (groups, removed in proto3) or 6/7 (never assigned).
    UnsupportedWireType { offset: usize, wire_type: u8 },
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Truncated { offset } => write!(f, "truncated at byte {offset}"),
            WireError::InvalidVarint { offset } => write!(f, "invalid varint at byte {offset}"),
            WireError::UnsupportedWireType { offset, wire_type } => {
                write!(f, "unsupported wire type {wire_type} at byte {offset}")
            }
        }
    }
}

impl std::error::Error for WireError {}

/// Frame a whole message. Fields keep wire order: a repeated field appears
/// once per element and, per protobuf's merge rule, the last occurrence of a
/// singular field wins.
pub fn read_fields(buf: &[u8]) -> Result<Vec<Field<'_>>, WireError> {
    let mut fields = Vec::new();
    let mut at = 0;
    while at < buf.len() {
        let tag_offset = at;
        let (tag, next) = read_varint(buf, at)?;
        at = next;
        let wire_type = (tag & 0x7) as u8;
        let number = u32::try_from(tag >> 3)
            .ok()
            .filter(|number| *number != 0)
            .ok_or(WireError::InvalidVarint { offset: tag_offset })?;
        let value = match wire_type {
            0 => {
                let (value, next) = read_varint(buf, at)?;
                at = next;
                WireValue::Varint(value)
            }
            1 => {
                let end = at.checked_add(8).filter(|end| *end <= buf.len());
                let end = end.ok_or(WireError::Truncated { offset: at })?;
                let mut raw = [0_u8; 8];
                raw.copy_from_slice(&buf[at..end]);
                at = end;
                WireValue::Fixed64(u64::from_le_bytes(raw))
            }
            2 => {
                let (len, next) = read_varint(buf, at)?;
                let len = usize::try_from(len).map_err(|_| WireError::Truncated { offset: at })?;
                let end = next.checked_add(len).filter(|end| *end <= buf.len());
                let end = end.ok_or(WireError::Truncated { offset: at })?;
                at = end;
                WireValue::Bytes(&buf[next..end])
            }
            5 => {
                let end = at.checked_add(4).filter(|end| *end <= buf.len());
                let end = end.ok_or(WireError::Truncated { offset: at })?;
                let mut raw = [0_u8; 4];
                raw.copy_from_slice(&buf[at..end]);
                at = end;
                WireValue::Fixed32(u32::from_le_bytes(raw))
            }
            other => {
                return Err(WireError::UnsupportedWireType {
                    offset: tag_offset,
                    wire_type: other,
                });
            }
        };
        fields.push(Field { number, value });
    }
    Ok(fields)
}

/// Elements of a `repeated` varint field. Proto3 packs repeated scalars by
/// default (one length-delimited run of varints), but a writer may also emit
/// one varint per element; both spellings land here.
pub fn repeated_varints(value: WireValue<'_>) -> Result<Vec<u64>, WireError> {
    match value {
        WireValue::Bytes(packed) => {
            let mut out = Vec::new();
            let mut at = 0;
            while at < packed.len() {
                let (element, next) = read_varint(packed, at)?;
                out.push(element);
                at = next;
            }
            Ok(out)
        }
        scalar => Ok(scalar.as_u64().into_iter().collect()),
    }
}

/// Base-128 varint, at most ten bytes (a 64-bit value).
fn read_varint(buf: &[u8], start: usize) -> Result<(u64, usize), WireError> {
    let mut value = 0_u64;
    let mut at = start;
    for shift in (0..64).step_by(7) {
        let byte = *buf.get(at).ok_or(WireError::Truncated { offset: start })?;
        at += 1;
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, at));
        }
        if shift == 63 {
            break;
        }
    }
    Err(WireError::InvalidVarint { offset: start })
}

/// Test-only encoder: enough of the wire format to build hermetic messages
/// for the typed decoders' unit tests and the usage normalizer's grammar
/// tests. Production code never encodes.
#[cfg(test)]
pub(crate) mod encode {
    pub fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    fn tag(number: u32, wire_type: u8) -> Vec<u8> {
        varint((u64::from(number) << 3) | u64::from(wire_type))
    }

    pub fn field_varint(number: u32, value: u64) -> Vec<u8> {
        let mut out = tag(number, 0);
        out.extend(varint(value));
        out
    }

    pub fn field_bytes(number: u32, bytes: &[u8]) -> Vec<u8> {
        let mut out = tag(number, 2);
        out.extend(varint(bytes.len() as u64));
        out.extend_from_slice(bytes);
        out
    }

    pub fn field_str(number: u32, text: &str) -> Vec<u8> {
        field_bytes(number, text.as_bytes())
    }

    pub fn field_fixed64(number: u32, value: u64) -> Vec<u8> {
        let mut out = tag(number, 1);
        out.extend_from_slice(&value.to_le_bytes());
        out
    }

    pub fn field_fixed32(number: u32, value: u32) -> Vec<u8> {
        let mut out = tag(number, 5);
        out.extend_from_slice(&value.to_le_bytes());
        out
    }

    pub fn packed_varints(number: u32, values: &[u64]) -> Vec<u8> {
        let payload: Vec<u8> = values.iter().flat_map(|value| varint(*value)).collect();
        field_bytes(number, &payload)
    }

    pub fn message(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.iter().flatten().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::encode::{
        field_bytes, field_fixed32, field_fixed64, field_str, field_varint, message,
        packed_varints, varint,
    };
    use super::{Field, WireError, WireValue, read_fields, repeated_varints};

    #[test]
    fn frames_every_supported_wire_type_in_order() {
        let buf = message(&[
            field_varint(1, 300),
            field_fixed64(2, 0x1122_3344_5566_7788),
            field_str(3, "gemini-3.7-flash"),
            field_fixed32(5, 7),
            field_varint(1, 1),
        ]);
        let fields = read_fields(&buf).unwrap();
        assert_eq!(
            fields,
            [
                Field {
                    number: 1,
                    value: WireValue::Varint(300)
                },
                Field {
                    number: 2,
                    value: WireValue::Fixed64(0x1122_3344_5566_7788)
                },
                Field {
                    number: 3,
                    value: WireValue::Bytes(b"gemini-3.7-flash")
                },
                Field {
                    number: 5,
                    value: WireValue::Fixed32(7)
                },
                Field {
                    number: 1,
                    value: WireValue::Varint(1)
                },
            ]
        );
        assert_eq!(fields[2].value.as_str(), Some("gemini-3.7-flash"));
        assert_eq!(fields[3].value.as_u64(), Some(7));
        assert_eq!(fields[2].value.as_u64(), None);
    }

    #[test]
    fn varints_cover_the_full_64_bit_range() {
        for value in [
            0,
            1,
            127,
            128,
            16_383,
            16_384,
            u64::from(u32::MAX),
            u64::MAX,
        ] {
            let encoded = field_varint(9, value);
            let fields = read_fields(&encoded).unwrap();
            assert_eq!(fields[0].value, WireValue::Varint(value));
        }
        let eleven_bytes: Vec<u8> = std::iter::once(0x48)
            .chain(std::iter::repeat_n(0xFF, 11))
            .collect();
        assert_eq!(
            read_fields(&eleven_bytes),
            Err(WireError::InvalidVarint { offset: 1 })
        );
    }

    #[test]
    fn repeated_scalars_accept_packed_and_unpacked_spellings() {
        let encoded = packed_varints(2, &[1, 300, 5]);
        let packed = read_fields(&encoded).unwrap();
        assert_eq!(repeated_varints(packed[0].value).unwrap(), [1, 300, 5]);
        let encoded = message(&[field_varint(2, 1), field_varint(2, 300)]);
        let unpacked = read_fields(&encoded).unwrap();
        let elements: Vec<u64> = unpacked
            .iter()
            .flat_map(|field| repeated_varints(field.value).unwrap())
            .collect();
        assert_eq!(elements, [1, 300]);
        let truncated = WireValue::Bytes(&[0x80]);
        assert!(repeated_varints(truncated).is_err());
    }

    #[test]
    fn malformed_buffers_are_errors_not_partial_messages() {
        assert_eq!(
            read_fields(&[0x08]),
            Err(WireError::Truncated { offset: 1 }),
            "tag without a value"
        );
        let mut cut = field_bytes(4, b"payload");
        cut.truncate(cut.len() - 2);
        assert_eq!(read_fields(&cut), Err(WireError::Truncated { offset: 1 }));
        assert_eq!(
            read_fields(&[0x0B]),
            Err(WireError::UnsupportedWireType {
                offset: 0,
                wire_type: 3
            }),
            "start-group"
        );
        assert_eq!(
            read_fields(&varint(0)),
            Err(WireError::InvalidVarint { offset: 0 }),
            "field number zero"
        );
        assert!(read_fields(&[]).unwrap().is_empty());
    }
}
