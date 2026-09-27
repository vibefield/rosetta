//! The one encoder of raw-record and seal frames.
//!
//! The ledger's own writers and callers that frame a batch before its segment
//! is known share this code, so equal inputs always become equal bytes. A
//! frame's checksum covers its file id, and a caller can only finish a frame
//! ahead of the ledger if that id is fixed in advance: with roster keys
//! installed ([`Ledger::set_source_keys`]) a source's id in every segment is
//! its key. [`PrebuiltFrames`] holds one source generation's finished frames;
//! [`Ledger::append_prebuilt`] writes them verbatim after the source's
//! define, exactly where the ledger's own framing would have put them.
//!
//! [`Ledger::set_source_keys`]: super::Ledger::set_source_keys
//! [`Ledger::append_prebuilt`]: super::Ledger::append_prebuilt

use super::{
    CHECKSUM_BYTES, CaptureError, MAX_FRAME_BYTES, TAG_GENERATION_COMPLETE, TAG_RAW, TAG_RAW_BATCH,
};

/// Small records of one capture batch share a frame up to this body size.
const BATCH_BODY_BYTES: usize = 1024 * 1024;
const BATCH_HEADER_BYTES: usize = 16;
const RECORD_HEADER_BYTES: usize = 12;
/// A record this large always gets a `TAG_RAW` frame of its own.
const LARGEST_BATCHED_PAYLOAD: usize = BATCH_BODY_BYTES - BATCH_HEADER_BYTES - RECORD_HEADER_BYTES;
pub(super) const RAW_FIXED_BYTES: usize = 20;
const SEAL_BODY_BYTES: usize = 17;
const HEAD_BYTES: usize = 5;

/// `tag ∙ u32 body-len`.
pub(super) fn frame_head(tag: u8, body_len: usize) -> [u8; HEAD_BYTES] {
    let mut head = [0_u8; HEAD_BYTES];
    head[0] = tag;
    head[1..5].copy_from_slice(&(body_len as u32).to_be_bytes());
    head
}

/// The first eight bytes of blake3 over the head and the body, in parts.
pub(super) fn frame_checksum(head: &[u8; HEAD_BYTES], body: &[&[u8]]) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(head);
    for part in body {
        hasher.update(part);
    }
    let mut checksum = [0_u8; CHECKSUM_BYTES];
    checksum.copy_from_slice(&hasher.finalize().as_bytes()[..CHECKSUM_BYTES]);
    checksum
}

/// A single-record `TAG_RAW` body's length, refused past the frame limit.
pub(super) fn raw_body_len(payload_len: usize) -> Result<usize, CaptureError> {
    let body_len = RAW_FIXED_BYTES.checked_add(payload_len).ok_or_else(|| {
        CaptureError::LimitExceeded("raw ledger record size overflowed usize".to_string())
    })?;
    if body_len > MAX_FRAME_BYTES {
        return Err(CaptureError::LimitExceeded(format!(
            "raw ledger record is {payload_len} bytes, frame limit is {}",
            MAX_FRAME_BYTES - RAW_FIXED_BYTES
        )));
    }
    Ok(body_len)
}

/// A single-record `TAG_RAW` body's fixed prefix; the payload follows it.
pub(super) fn raw_fixed(file_id: u32, generation: u64, start_offset: u64) -> [u8; RAW_FIXED_BYTES] {
    let mut fixed = [0_u8; RAW_FIXED_BYTES];
    fixed[0..4].copy_from_slice(&file_id.to_be_bytes());
    fixed[4..12].copy_from_slice(&generation.to_be_bytes());
    fixed[12..20].copy_from_slice(&start_offset.to_be_bytes());
    fixed
}

/// A `TAG_GENERATION_COMPLETE` body.
pub(super) fn seal_body(
    file_id: u32,
    generation: u64,
    normalizer_version: u32,
    normalized_clean: bool,
) -> [u8; SEAL_BODY_BYTES] {
    let mut body = [0_u8; SEAL_BODY_BYTES];
    body[0..4].copy_from_slice(&file_id.to_be_bytes());
    body[4..12].copy_from_slice(&generation.to_be_bytes());
    body[12..16].copy_from_slice(&normalizer_version.to_be_bytes());
    body[16] = u8::from(normalized_clean);
    body
}

/// One source generation's finished frames, built before the segment they
/// land in is known. Their file id is the source's roster key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrebuiltFrames {
    key: u32,
    generation: u64,
    bytes: Vec<u8>,
    frames: u64,
    seals: bool,
}

impl PrebuiltFrames {
    pub fn new(key: u32, generation: u64) -> Self {
        Self {
            key,
            generation,
            bytes: Vec::new(),
            frames: 0,
            seals: false,
        }
    }

    pub fn key(&self) -> u32 {
        self.key
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Frames built so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Frame bytes built so far, heads and checksums included.
    pub fn frame_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// Whether the last frame seals the generation.
    pub fn seals(&self) -> bool {
        self.seals
    }

    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Frame one capture batch's records: small records share `TAG_RAW_BATCH`
    /// frames of at most 1 MiB of body, a record too large for one gets its
    /// own `TAG_RAW` frame, and a batch of one record is demoted to `TAG_RAW`.
    /// Payloads are copied once, into the finished frames.
    pub fn push_records(&mut self, records: Vec<(u64, Vec<u8>)>) -> Result<(), CaptureError> {
        self.check_open();
        if records.len() <= 1 {
            for (offset, payload) in records {
                self.push_record(offset, &payload)?;
            }
            return Ok(());
        }
        self.bytes
            .reserve(records.iter().fold(0_usize, |bytes, (_, payload)| {
                bytes
                    .saturating_add(RECORD_HEADER_BYTES + payload.len())
                    .saturating_add(HEAD_BYTES + BATCH_HEADER_BYTES + CHECKSUM_BYTES)
            }));
        let mut open: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut body_len = 0_usize;
        for (offset, payload) in records {
            if payload.len() > LARGEST_BATCHED_PAYLOAD {
                self.push_batch(&mut open, body_len)?;
                self.push_record(offset, &payload)?;
                continue;
            }
            if !open.is_empty() && body_len + RECORD_HEADER_BYTES + payload.len() > BATCH_BODY_BYTES
            {
                self.push_batch(&mut open, body_len)?;
            }
            if open.is_empty() {
                body_len = BATCH_HEADER_BYTES;
            }
            body_len += RECORD_HEADER_BYTES + payload.len();
            open.push((offset, payload));
        }
        self.push_batch(&mut open, body_len)
    }

    /// Frame one record alone, as `TAG_RAW`.
    pub fn push_record(&mut self, start_offset: u64, payload: &[u8]) -> Result<(), CaptureError> {
        self.check_open();
        let body_len = raw_body_len(payload.len())?;
        let fixed = raw_fixed(self.key, self.generation, start_offset);
        let head = frame_head(TAG_RAW, body_len);
        let checksum = frame_checksum(&head, &[&fixed, payload]);
        self.bytes.reserve(HEAD_BYTES + body_len + CHECKSUM_BYTES);
        self.bytes.extend_from_slice(&head);
        self.bytes.extend_from_slice(&fixed);
        self.bytes.extend_from_slice(payload);
        self.bytes.extend_from_slice(&checksum);
        self.frames += 1;
        Ok(())
    }

    /// Seal the generation after every record framed so far. Nothing may
    /// follow a seal.
    pub fn push_seal(&mut self, normalizer_version: u32, normalized_clean: bool) {
        self.check_open();
        let body = seal_body(
            self.key,
            self.generation,
            normalizer_version,
            normalized_clean,
        );
        let head = frame_head(TAG_GENERATION_COMPLETE, body.len());
        let checksum = frame_checksum(&head, &[&body]);
        self.bytes.extend_from_slice(&head);
        self.bytes.extend_from_slice(&body);
        self.bytes.extend_from_slice(&checksum);
        self.frames += 1;
        self.seals = true;
    }

    fn check_open(&self) {
        assert!(!self.seals, "a sealed generation takes no more frames");
    }

    /// Close the open batch: one `TAG_RAW_BATCH` frame, or a `TAG_RAW` frame
    /// when it holds a single record.
    fn push_batch(
        &mut self,
        open: &mut Vec<(u64, Vec<u8>)>,
        body_len: usize,
    ) -> Result<(), CaptureError> {
        match open.len() {
            0 => return Ok(()),
            1 => {
                let (offset, payload) = open.pop().expect("one record");
                return self.push_record(offset, &payload);
            }
            _ => {}
        }
        let start = self.bytes.len();
        self.bytes
            .extend_from_slice(&frame_head(TAG_RAW_BATCH, body_len));
        self.bytes.extend_from_slice(&self.key.to_be_bytes());
        self.bytes.extend_from_slice(&self.generation.to_be_bytes());
        self.bytes
            .extend_from_slice(&(open.len() as u32).to_be_bytes());
        for (offset, payload) in open.drain(..) {
            self.bytes.extend_from_slice(&offset.to_be_bytes());
            self.bytes
                .extend_from_slice(&(payload.len() as u32).to_be_bytes());
            self.bytes.extend_from_slice(&payload);
        }
        debug_assert_eq!(self.bytes.len() - start, HEAD_BYTES + body_len);
        let checksum = blake3::hash(&self.bytes[start..]);
        self.bytes
            .extend_from_slice(&checksum.as_bytes()[..CHECKSUM_BYTES]);
        self.frames += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
