//! Reattach replayed JSONL cursors to verified native bytes before cold catch-up.
//! The ledger knows record positions and payloads, but not native file identity
//! or line endings. The ordinary tailer supplies those only after verification.

use std::collections::BTreeMap;
use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, StreamDriver};
use rosetta_capture::{TailCheckpoint, TailItem, TailRead, Tailer, Transition};
use rosetta_core::rows::RuntimeRow;
use rosetta_store::{Batch, CheckpointUpdate, Store, queries};

use crate::{IngestOptions, Instrument, Metric, PipeCheckpoint, PipelineError, SessionFile};

#[derive(Debug, Default)]
pub struct RestoredPrefixes(BTreeMap<String, Prefix>);

#[derive(Debug, Default)]
struct Prefix {
    // Fixed-size per source even when thousands of histories are interleaved.
    hash: [u8; 32],
    payload_end: u64,
    records: u64,
    payload_bytes: u64,
    generation: u64,
}

impl Prefix {
    fn record(&mut self, start: u64, payload: &[u8]) {
        let len = payload.len() as u64;
        let mut next = blake3::Hasher::new();
        next.update(&self.hash);
        next.update(&start.to_le_bytes());
        next.update(&len.to_le_bytes());
        next.update(payload);
        self.hash = *next.finalize().as_bytes();
        self.payload_end = start.saturating_add(len);
        self.records += 1;
        self.payload_bytes += len;
    }
}

impl RestoredPrefixes {
    pub(super) fn record(&mut self, path: &str, generation: u64, offset: u64, payload: &[u8]) {
        let prefix = match self.0.get_mut(path) {
            Some(prefix) => prefix,
            None => self.0.entry(path.to_owned()).or_default(),
        };
        prefix.generation = generation;
        prefix.record(offset, payload);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn bind(
        self,
        adapter: &dyn AgentAdapter,
        runtime: &RuntimeRow,
        root: &Path,
        roster: &[SessionFile],
        store: &mut Store,
        opts: &IngestOptions,
        instrument: &Instrument,
    ) -> Result<crate::StreamReport, PipelineError> {
        let mut verified = crate::StreamReport::default();
        let tailer = Tailer::new(opts.tail.clone())?;
        for file in roster {
            opts.check_cancelled()?;
            let Some(prefix) = self.0.get(&file.relative_path) else {
                continue;
            };
            if file.driver != StreamDriver::JsonLines {
                continue;
            }
            let Some(cursor) =
                queries::cursor_state(store.conn(), &runtime.id, &file.relative_path)?
            else {
                continue;
            };
            let Some(mut checkpoint) = PipeCheckpoint::decode(&cursor.checkpoint) else {
                continue;
            };
            if cursor.active_generation != prefix.generation
                || checkpoint.source_generation != prefix.generation
                || checkpoint.normalizer_version != adapter.normalizer_version()
                || checkpoint.session_id != file.session_id
                || checkpoint.stream_id != file.stream_id
                || checkpoint.tail.is_some()
            {
                continue;
            }
            match prefix.verify(root, file, &tailer, opts, instrument)? {
                Some((tail, stamp)) => {
                    checkpoint.tail = Some(tail.encode_hex());
                    checkpoint.snapshot_stamp = stamp;
                    verified.payload_bytes += prefix.payload_bytes;
                    verified.records += prefix.records;
                    instrument.add(Metric::PayloadBytes, prefix.payload_bytes);
                    instrument.add(Metric::RecordsFramed, prefix.records);
                    if let Some(progress) = &opts.progress {
                        progress.source_bytes(file, prefix.payload_bytes);
                    }
                }
                None => {
                    // The projected generation stays active until its replacement
                    // is complete. A fresh cursor above a live generation makes
                    // the normal writer stage, activate and sweep the old rows.
                    checkpoint = PipeCheckpoint::fresh(
                        &file.session_id,
                        &file.stream_id,
                        adapter.normalizer_version(),
                    );
                }
            }
            instrument.apply(
                store,
                &Batch {
                    ops: vec![],
                    source: None,
                    checkpoints: vec![CheckpointUpdate {
                        runtime_id: runtime.id.clone(),
                        stream: file.stream_id.clone(),
                        path: file.relative_path.clone(),
                        session_id: file.projected_session_id(),
                        expected: Some(cursor.checkpoint),
                        next: checkpoint.encode(),
                        updated_at: None,
                    }],
                },
            )?;
        }
        Ok(verified)
    }
}

impl Prefix {
    fn verify(
        &self,
        root: &Path,
        file: &SessionFile,
        tailer: &Tailer,
        opts: &IngestOptions,
        instrument: &Instrument,
    ) -> Result<Option<(TailCheckpoint, Option<String>)>, PipelineError> {
        let mut input = match &file.input {
            Some(source) => match source.open(root, || opts.check_cancelled().is_err()) {
                Ok(Some(mut input)) => {
                    input.cancel = opts.cancel.clone();
                    Some(input)
                }
                Ok(None) => return Ok(None),
                Err(error) if error.incomplete_input() => return Ok(None),
                Err(error) => {
                    opts.check_cancelled()?;
                    return Err(error.into());
                }
            },
            None => None,
        };
        let path = root.join(&file.relative_path);
        let lf = self.payload_end.saturating_add(1);
        let mut end = lf;
        let mut previous = None;
        let mut actual = Prefix::default();
        loop {
            opts.check_cancelled()?;
            let read = match &mut input {
                Some(input) => tailer.read_input(input, previous.as_ref(), false, Some(end)),
                None => tailer.read_cut(&path, previous.as_ref(), false, Some(end)),
            }?;
            let TailRead::Batch(batch) = read else {
                return Ok(None);
            };
            instrument.add(Metric::BytesRead, batch.bytes_read);
            if previous.is_some() && batch.transition != Transition::Continued {
                return Ok(None);
            }
            for item in batch.items {
                let TailItem::Record(record) = item else {
                    return Ok(None);
                };
                actual.record(record.start_offset, &record.payload);
            }
            if batch.checkpoint.committed_offset == end {
                return Ok((actual.hash == self.hash)
                    .then(|| (batch.checkpoint, input.map(|input| input.stamp))));
            }
            if !batch.more_available {
                // CRLF normalization removes one extra physical byte. Permit
                // that delimiter without consuming any following record.
                if end != lf || batch.snapshot_len != end {
                    return Ok(None);
                }
                end = lf.saturating_add(1);
            }
            previous = Some(batch.checkpoint);
        }
    }
}
