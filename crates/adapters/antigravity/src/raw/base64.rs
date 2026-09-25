//! Standard base64 (RFC 4648, `+/`, `=` padding) — the encoding the capture
//! layer uses for SQLite blob columns (`{"$base64": "..."}`) in snapshot
//! records. Hand-rolled so the adapter does not gain a dependency for thirty
//! lines; `encode` exists so replay tests can frame fixture rows exactly the
//! way capture does.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Decode strictly: only the standard alphabet, canonical padding, and a
/// length that is a multiple of four. Anything else answers `None`, which the
/// normalizer reports as a hard parse failure rather than a partial blob.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index + 1 == bytes.len() / 4;
        let mut sextets = [0_u32; 4];
        let mut padding = 0;
        for (at, byte) in chunk.iter().enumerate() {
            if *byte == b'=' {
                // Padding only closes the final quantum, in its last two slots.
                if !last || at < 2 {
                    return None;
                }
                padding += 1;
                continue;
            }
            if padding > 0 {
                return None;
            }
            sextets[at] = sextet(*byte)?;
        }
        let n = (sextets[0] << 18) | (sextets[1] << 12) | (sextets[2] << 6) | sextets[3];
        out.push((n >> 16) as u8);
        if padding < 2 {
            out.push((n >> 8) as u8);
        }
        if padding < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Encode the way `rosetta-capture` serializes blobs, so a test can build a
/// byte-identical snapshot record from a fixture database row.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for at in 0..4 {
            if at <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * at)) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn sextet(byte: u8) -> Option<u32> {
    let value = match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(value))
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn round_trips_every_padding_length() {
        for len in 0..=9 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let text = encode(&bytes);
            assert_eq!(text.len(), bytes.len().div_ceil(3) * 4);
            assert_eq!(decode(&text).as_deref(), Some(bytes.as_slice()), "{text}");
        }
    }

    #[test]
    fn matches_the_capture_layer_vectors() {
        assert_eq!(encode(&[0x01, 0x02]), "AQI=");
        assert_eq!(decode("AQI=").unwrap(), [0x01, 0x02]);
        assert_eq!(encode(b"Man"), "TWFu");
        assert_eq!(decode("TWE=").unwrap(), b"Ma");
        assert_eq!(decode("").unwrap(), b"");
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in ["AQI", "AQ=I", "====", "A===", "AQI=AQI=", "AQI-", "AQ I"] {
            assert!(decode(bad).is_none(), "{bad:?} decoded");
        }
    }
}
