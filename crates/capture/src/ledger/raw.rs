//! Bounded raw-record framing shared by synchronous and asynchronous writers.

use std::io::Write;
use std::time::Instant;

use super::{
    CHECKSUM_BYTES, CaptureError, Ledger, MAX_FRAME_BYTES, TAG_RAW, TAG_RAW_BATCH, elapsed_nanos,
    io_error, segment_name,
};

const BATCH_BODY_BYTES: usize = 1024 * 1024;
const BATCH_HEADER_BYTES: usize = 16;
const RECORD_HEADER_BYTES: usize = 12;

impl Ledger {
    pub(super) fn raw_owned_batch(
        &mut self,
        file_id: u32,
        generation: u64,
        records: Vec<(u64, Vec<u8>)>,
    ) -> Result<(), CaptureError> {
        let before = self
            .write_stats
            .frame_nanos
            .saturating_add(self.write_stats.buffer_write_nanos);
        let started = Instant::now();
        let result = self.write_owned_batch(file_id, generation, records);
        let nested = self
            .write_stats
            .frame_nanos
            .saturating_add(self.write_stats.buffer_write_nanos)
            .saturating_sub(before);
        // Frame methods already account for checksums and writes. Attribute
        // packing once per capture batch without a clock read per record or
        // double-counting the nested writer work.
        self.write_stats.frame_nanos = self
            .write_stats
            .frame_nanos
            .saturating_add(elapsed_nanos(started).saturating_sub(nested));
        result
    }

    fn write_owned_batch(
        &mut self,
        file_id: u32,
        generation: u64,
        records: Vec<(u64, Vec<u8>)>,
    ) -> Result<(), CaptureError> {
        if records.len() <= 1 {
            for (offset, payload) in records {
                self.raw_frame(file_id, generation, offset, &payload)?;
            }
            return Ok(());
        }
        let capacity = records
            .iter()
            .fold(BATCH_HEADER_BYTES, |bytes, (_, payload)| {
                bytes
                    .saturating_add(RECORD_HEADER_BYTES)
                    .saturating_add(payload.len())
            })
            .min(BATCH_BODY_BYTES);
        let mut body = Vec::with_capacity(capacity);
        let mut count = 0_u32;
        for (offset, payload) in records {
            if payload.len() > BATCH_BODY_BYTES - BATCH_HEADER_BYTES - RECORD_HEADER_BYTES {
                self.finish_raw_batch(file_id, generation, &mut body, &mut count)?;
                self.raw_frame(file_id, generation, offset, &payload)?;
                continue;
            }
            if !body.is_empty()
                && body.len() + RECORD_HEADER_BYTES + payload.len() > BATCH_BODY_BYTES
            {
                self.finish_raw_batch(file_id, generation, &mut body, &mut count)?;
            }
            if body.is_empty() {
                body.extend_from_slice(&file_id.to_be_bytes());
                body.extend_from_slice(&generation.to_be_bytes());
                body.extend_from_slice(&0_u32.to_be_bytes());
            }
            body.extend_from_slice(&offset.to_be_bytes());
            body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            body.extend_from_slice(&payload);
            count += 1;
        }
        self.finish_raw_batch(file_id, generation, &mut body, &mut count)
    }

    fn finish_raw_batch(
        &mut self,
        file_id: u32,
        generation: u64,
        body: &mut Vec<u8>,
        count: &mut u32,
    ) -> Result<(), CaptureError> {
        match *count {
            0 => return Ok(()),
            1 => {
                let offset = u64::from_be_bytes(body[16..24].try_into().expect("record offset"));
                self.raw_frame(file_id, generation, offset, &body[28..])?;
            }
            _ => {
                body[12..16].copy_from_slice(&count.to_be_bytes());
                self.note_generation(file_id, generation);
                self.frame(TAG_RAW_BATCH, body)?;
            }
        }
        body.clear();
        *count = 0;
        Ok(())
    }

    pub(super) fn raw_frame(
        &mut self,
        file_id: u32,
        generation: u64,
        start_offset: u64,
        payload: &[u8],
    ) -> Result<(), CaptureError> {
        const FIXED_BYTES: usize = 20;
        let body_len = FIXED_BYTES.checked_add(payload.len()).ok_or_else(|| {
            CaptureError::LimitExceeded("raw ledger record size overflowed usize".to_string())
        })?;
        if body_len > MAX_FRAME_BYTES {
            return Err(CaptureError::LimitExceeded(format!(
                "raw ledger record is {} bytes, frame limit is {}",
                payload.len(),
                MAX_FRAME_BYTES - FIXED_BYTES
            )));
        }
        self.note_generation(file_id, generation);
        self.ensure_writer()?;
        let path = self.dir.join(segment_name(self.segment_seq));
        let started = Instant::now();
        let mut head = [0_u8; 5];
        head[0] = TAG_RAW;
        head[1..5].copy_from_slice(&(body_len as u32).to_be_bytes());
        let mut fixed = [0_u8; FIXED_BYTES];
        fixed[0..4].copy_from_slice(&file_id.to_be_bytes());
        fixed[4..12].copy_from_slice(&generation.to_be_bytes());
        fixed[12..20].copy_from_slice(&start_offset.to_be_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(&fixed);
        hasher.update(payload);
        let digest = hasher.finalize();
        let checksum = &digest.as_bytes()[..CHECKSUM_BYTES];
        self.write_stats.frame_nanos = self
            .write_stats
            .frame_nanos
            .saturating_add(elapsed_nanos(started));

        let started = Instant::now();
        let written = {
            let writer = self.writer.as_mut().expect("ensure_writer just ran");
            writer
                .write_all(&head)
                .and_then(|_| writer.write_all(&fixed))
                .and_then(|_| writer.write_all(payload))
                .and_then(|_| writer.write_all(checksum))
        };
        self.write_stats.buffer_write_nanos = self
            .write_stats
            .buffer_write_nanos
            .saturating_add(elapsed_nanos(started));
        written.map_err(|error| io_error("appending to", &path, error))?;

        let frame_bytes = (head.len() + body_len + CHECKSUM_BYTES) as u64;
        self.write_stats.frames = self.write_stats.frames.saturating_add(1);
        self.write_stats.frame_bytes = self.write_stats.frame_bytes.saturating_add(frame_bytes);
        self.written += frame_bytes;
        Ok(())
    }
}
