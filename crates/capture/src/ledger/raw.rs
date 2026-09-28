//! Raw-record writes shared by synchronous and asynchronous writers. Every
//! raw frame's bytes come from the shared encoder in `frames.rs`.

use std::io::Write;
use std::time::Instant;

use super::frames::{PrebuiltFrames, frame_checksum, frame_head, raw_body_len, raw_fixed};
use super::{CHECKSUM_BYTES, CaptureError, Ledger, TAG_RAW, elapsed_nanos, io_error, segment_name};

impl Ledger {
    /// Frame one capture batch — small records share bounded batch frames —
    /// and write it.
    pub(super) fn raw_owned_batch(
        &mut self,
        file_id: u32,
        generation: u64,
        records: Vec<(u64, Vec<u8>)>,
    ) -> Result<(), CaptureError> {
        let started = Instant::now();
        let mut frames = PrebuiltFrames::new(file_id, generation);
        let framed = frames.push_records(records);
        self.write_stats.frame_nanos = self
            .write_stats
            .frame_nanos
            .saturating_add(elapsed_nanos(started));
        framed?;
        self.write_frames(file_id, frames)
    }

    /// Write finished frames of a source this segment has defined as
    /// `file_id`, verbatim. A zstd-frames segment first compresses frames
    /// that arrive plain exactly as a capture worker would have, so its bytes
    /// do not depend on who compressed them.
    pub(super) fn write_frames(
        &mut self,
        file_id: u32,
        mut frames: PrebuiltFrames,
    ) -> Result<(), CaptureError> {
        if frames.is_empty() {
            return Ok(());
        }
        self.note_generation(file_id, frames.generation());
        self.ensure_writer()?;
        let path = self.dir.join(segment_name(self.segment_seq));
        let writer = self.writer.as_mut().expect("ensure_writer just ran");
        if let Some(compressor) = writer.batch_compressor() {
            let started = Instant::now();
            let compressed = frames.compress(compressor);
            self.write_stats.compress_nanos = self
                .write_stats
                .compress_nanos
                .saturating_add(elapsed_nanos(started));
            compressed?;
        }
        let started = Instant::now();
        let (compressed, plain) = frames.parts();
        let written = compressed
            .map_or(Ok(()), |frame| writer.write_zstd_frame(frame))
            .and_then(|()| writer.write_all(plain));
        self.write_stats.buffer_write_nanos = self
            .write_stats
            .buffer_write_nanos
            .saturating_add(elapsed_nanos(started));
        written.map_err(|error| io_error("appending to", &path, error))?;

        let frame_bytes = frames.frame_bytes();
        self.write_stats.frames = self.write_stats.frames.saturating_add(frames.frames());
        self.write_stats.frame_bytes = self.write_stats.frame_bytes.saturating_add(frame_bytes);
        self.written += frame_bytes;
        Ok(())
    }

    /// One record as its own `TAG_RAW` frame, streamed to the segment writer
    /// without copying the payload into a frame buffer first.
    pub(super) fn raw_frame(
        &mut self,
        file_id: u32,
        generation: u64,
        start_offset: u64,
        payload: &[u8],
    ) -> Result<(), CaptureError> {
        let body_len = raw_body_len(payload.len())?;
        self.note_generation(file_id, generation);
        self.ensure_writer()?;
        let path = self.dir.join(segment_name(self.segment_seq));
        let started = Instant::now();
        let head = frame_head(TAG_RAW, body_len);
        let fixed = raw_fixed(file_id, generation, start_offset);
        let checksum = frame_checksum(&head, &[&fixed, payload]);
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
                .and_then(|_| writer.write_all(&checksum))
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
