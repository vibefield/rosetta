//! Bounded binary reader for durable tail checkpoints.
use super::*;

pub(super) struct ByteReader<'a> {
    remaining: &'a [u8],
}

impl<'a> ByteReader<'a> {
    /// Accept any version in `versions`, returning the one found so the
    /// caller can decode the fields that version carries.
    pub(super) fn versioned(
        bytes: &'a [u8],
        magic: &[u8],
        versions: std::ops::RangeInclusive<u8>,
    ) -> Result<(Self, u8), CaptureError> {
        let Some(rest) = bytes.strip_prefix(magic) else {
            return Err(CaptureError::InvalidCheckpoint(
                "checkpoint magic does not match".to_string(),
            ));
        };
        let mut reader = Self { remaining: rest };
        let version = reader.byte()?;
        if !versions.contains(&version) {
            return Err(CaptureError::InvalidCheckpoint(
                "unsupported checkpoint version".to_string(),
            ));
        }
        Ok((reader, version))
    }

    pub(super) fn finish(self) -> Result<(), CaptureError> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(CaptureError::InvalidCheckpoint(
                "checkpoint contains trailing bytes".to_string(),
            ))
        }
    }

    pub(super) fn byte(&mut self) -> Result<u8, CaptureError> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u64(&mut self) -> Result<u64, CaptureError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(
            |_| CaptureError::InvalidCheckpoint("truncated u64".to_string()),
        )?))
    }

    pub(super) fn len_u64(&mut self) -> Result<usize, CaptureError> {
        usize::try_from(self.u64()?).map_err(|_| {
            CaptureError::InvalidCheckpoint("checkpoint length is too large".to_string())
        })
    }

    pub(super) fn hash(&mut self) -> Result<[u8; HASH_BYTES], CaptureError> {
        self.take(HASH_BYTES)?
            .try_into()
            .map_err(|_| CaptureError::InvalidCheckpoint("truncated hash".to_string()))
    }

    pub(super) fn take(&mut self, count: usize) -> Result<&'a [u8], CaptureError> {
        if count > self.remaining.len() {
            return Err(CaptureError::InvalidCheckpoint(
                "checkpoint is truncated".to_string(),
            ));
        }
        let (value, rest) = self.remaining.split_at(count);
        self.remaining = rest;
        Ok(value)
    }
}
