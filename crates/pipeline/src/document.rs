//! Stream phase for rewrite-in-place document snapshots.
//!
//! Physical documents are read once per pass. Each native member receives a
//! virtual source and then enters the same staged-generation projector used by
//! SQLite snapshots.

use std::collections::BTreeMap;
use std::path::Path;

use rosetta_adapter_support::AgentAdapter;
use rosetta_store::Store;

use crate::instrument::{Instrument, Metric};
use crate::meta::SessionFile;
use crate::snapshot::apply_snapshot_records;
use crate::stream::{StreamReport, cursor_row};
use crate::{IngestOptions, PipelineError};

pub(crate) fn virtual_document_path(relative_path: &str) -> Option<&str> {
    relative_path.split_once('#').map(|(document, _)| document)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_document_sources(
    adapter: &dyn AgentAdapter,
    runtime: &rosetta_core::rows::RuntimeRow,
    root: &Path,
    store: &mut Store,
    files: &[&SessionFile],
    opts: &IngestOptions,
    instrument: &Instrument,
    ledger: &mut Option<&mut rosetta_capture::Ledger>,
    report: &mut StreamReport,
    file_health: &mut BTreeMap<String, bool>,
    session_health: &mut BTreeMap<String, bool>,
) -> Result<(), PipelineError> {
    let mut by_document: BTreeMap<&str, Vec<&SessionFile>> = BTreeMap::new();
    for file in files {
        let Some(document) = virtual_document_path(&file.relative_path) else {
            return Err(PipelineError::Config(format!(
                "document snapshot source {} has no #session suffix",
                file.relative_path
            )));
        };
        by_document.entry(document).or_default().push(file);
    }

    for (relative_path, group) in by_document {
        report.files += group.len();
        let mark = |file: &SessionFile,
                    complete: bool,
                    file_health: &mut BTreeMap<String, bool>,
                    session_health: &mut BTreeMap<String, bool>| {
            file_health.insert(file.relative_path.clone(), complete);
            session_health
                .entry(file.session_id.clone())
                .and_modify(|healthy| *healthy &= complete);
        };

        let path = root.join(relative_path);
        let document_limit =
            adapter.document_size_limit(&group[0].stream_id, opts.tail.max_record_bytes);
        let mut retries = 0_u32;
        let snapshot = loop {
            match instrument.time(Metric::ReadNanos, || {
                rosetta_capture::snapshot_document(&path, document_limit)
            })? {
                rosetta_capture::DocumentOutcome::Ready(snapshot) => break Some(snapshot),
                rosetta_capture::DocumentOutcome::Missing => {
                    report.files_missing += group.len();
                    break None;
                }
                rosetta_capture::DocumentOutcome::Unstable => {
                    retries += 1;
                    if retries > opts.max_retries {
                        report.retries_exhausted += group.len() as u64;
                        break None;
                    }
                }
                rosetta_capture::DocumentOutcome::Oversized { .. } => {
                    report.oversized += group.len() as u64;
                    break None;
                }
            }
        };
        let Some(snapshot) = snapshot else {
            if group.iter().all(|file| file.source_missing) {
                for file in &group {
                    let cursor = cursor_row(adapter, store, runtime, file, instrument)?;
                    let complete = apply_snapshot_records(
                        adapter,
                        runtime,
                        store,
                        file,
                        &cursor,
                        &[],
                        "missing",
                        ledger,
                        instrument,
                        report,
                    )?;
                    mark(file, complete, file_health, session_health);
                }
                continue;
            }
            for file in &group {
                mark(file, false, file_health, session_health);
            }
            continue;
        };

        for file in group {
            let cursor = cursor_row(adapter, store, runtime, file, instrument)?;
            let records = adapter.document_records(
                &file.stream_id,
                &file.native_session_id,
                snapshot.bytes(),
            );
            let complete = apply_snapshot_records(
                adapter,
                runtime,
                store,
                file,
                &cursor,
                &records,
                snapshot.stamp(),
                ledger,
                instrument,
                report,
            )?;
            mark(file, complete, file_health, session_health);
        }
    }
    Ok(())
}
