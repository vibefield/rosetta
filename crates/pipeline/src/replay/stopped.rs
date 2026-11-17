//! One source generation replayed on its own, and the record step it shares
//! with the ledger-order replay.
//!
//! A fresh private build that loses a file mid-read ends that generation at
//! the last record it read, activates it, and seals it in the raw ledger
//! (`stop_short`): the session publishes `Streaming` with what was read, and
//! the published ledger replays the same rows. A resumed build meets the same
//! loss as a raw-only generation in its staged ledger whose file no longer
//! reads into the roster. [`activate_stopped_source`] closes it exactly so,
//! from the staged frames, which is what keeps that archive rebuildable:
//! `rebuild` refuses a raw-only source it cannot prove retired.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rosetta_adapter_support::{AgentAdapter, LineStatus};
use rosetta_capture::{Ledger, LedgerEntry, SourceDef};
use rosetta_core::rows::RuntimeRow;
use rosetta_store::{SourceMode, Store, queries};

use super::{
    AdmissionScan, FileReplay, RebuildOptions, RebuildReport, flush_chunk, open_file,
    over_projection_limit, quarantined_record,
};
use crate::PipelineError;
use crate::instrument::{Instrument, Metric};

/// How [`activate_stopped_source`] closed an interrupted generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoppedSource {
    /// Raw records the staged prefix held.
    pub records: u64,
    /// Whether every one normalized. The seal records it; the caller holds
    /// the session `Streaming` either way, since the source was cut short.
    pub generation_clean: bool,
}

/// Close a private build's interrupted `generation` of `source`, whose file
/// is gone, at the last record the staged ledger holds: replay exactly that
/// generation's frames (the segment index narrows the walk), append its seal
/// to `ledger`, then activate it at the generation it was captured under —
/// by the end-of-file rule of a private build, unclean or not. A later
/// `rebuild` elects that seal and projects the same rows. The caller decides
/// readiness and must not also fence or reread the source.
#[allow(clippy::too_many_arguments)]
pub fn activate_stopped_source(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    ledger_dir: &Path,
    store: &mut Store,
    source: &SourceDef,
    generation: u64,
    ledger: &mut Ledger,
    instrument: &Instrument,
    cancel: Option<&AtomicBool>,
) -> Result<StoppedSource, PipelineError> {
    let path = &source.relative_path;
    if let Some(cursor) = queries::cursor_state(store.conn(), &runtime.id, path)?
        && cursor.highest_generation >= generation
    {
        return Err(PipelineError::Config(format!(
            "cannot close {path}: generation {generation} is already superseded"
        )));
    }
    let chunk_ops = RebuildOptions::default().chunk_ops;
    let mut report = RebuildReport::default();
    // Session metadata was replayed or skimmed already; stub only a session
    // no metadata named, as the ledger-order replay does.
    let mut sessions_seen = HashSet::new();
    if queries::session(store.conn(), &source.session_id)?.is_some() {
        sessions_seen.insert(source.session_id.clone());
    }
    let mut admission = None;
    let mut file: Option<FileReplay> = None;
    let filter = rosetta_capture::segment_filter(ledger_dir, &source.session_id, path, generation)?;
    rosetta_capture::visit_ledger_segments::<PipelineError, _>(
        ledger_dir,
        |segment| filter.admits(segment),
        |entry| {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                return Err(PipelineError::Cancelled);
            }
            match entry {
                LedgerEntry::Raw {
                    source: mut raw,
                    generation: raw_generation,
                    start_offset,
                    payload,
                } if raw.relative_path == *path && raw_generation == generation => {
                    // The current adapter's admission, decided by the opening
                    // record, exactly as the ledger-order replay decides it.
                    let admission = admission.get_or_insert_with(|| {
                        let mut scan = AdmissionScan::default();
                        scan.observe(adapter, &raw, generation, start_offset, &payload);
                        scan.finish(&BTreeMap::from([(
                            path.clone(),
                            (generation, source.clone()),
                        )]))
                    });
                    admission.project_source(&mut raw);
                    if file.is_none() {
                        file = Some(open_file(
                            adapter,
                            runtime,
                            &raw,
                            generation,
                            store,
                            &mut sessions_seen,
                            &mut report,
                            instrument,
                        )?);
                    }
                    let file = file.as_mut().expect("opened above");
                    project_raw(
                        adapter,
                        runtime,
                        file,
                        &raw,
                        start_offset,
                        &payload,
                        &mut report,
                        instrument,
                    );
                    if file.ops.len() >= chunk_ops {
                        flush_chunk(
                            adapter,
                            runtime,
                            file,
                            SourceMode::Stage,
                            false,
                            store,
                            &mut report,
                            instrument,
                        )?;
                    }
                }
                LedgerEntry::Document {
                    source: mut document_source,
                    generation: document_generation,
                    start_offset,
                    document,
                } if document_source.relative_path == *path
                    && document_generation == generation =>
                {
                    if let Some(admission) = &admission {
                        admission.project_source(&mut document_source);
                    }
                    if !document_source.ledger_only
                        && let Some(file) = file.as_mut()
                    {
                        crate::presented_media::project_document(
                            &document_source.session_id,
                            start_offset,
                            &document,
                            &mut file.slots,
                            &mut file.ops,
                        );
                    }
                }
                _ => {}
            }
            Ok(())
        },
    )?;
    let mut file = match file {
        Some(file) => file,
        None => open_file(
            adapter,
            runtime,
            source,
            generation,
            store,
            &mut sessions_seen,
            &mut report,
            instrument,
        )?,
    };
    // Ledger first, as the pass that cut the source short would have: the
    // seal certifies the raw prefix and records how it normalized.
    instrument.ledger_write(ledger, |ledger| {
        ledger.complete_generation(
            source,
            generation,
            adapter.normalizer_version(),
            file.generation_clean,
        )
    })?;
    flush_chunk(
        adapter,
        runtime,
        &mut file,
        SourceMode::Activate,
        true,
        store,
        &mut report,
        instrument,
    )?;
    Ok(StoppedSource {
        records: report.raw_records,
        generation_clean: file.generation_clean,
    })
}

/// Normalize one raw frame into its file's pending ops: a repeated source
/// position is skipped (`false`), a record past the projection limit is
/// quarantined, and a hard parse error marks the generation unclean.
#[allow(clippy::too_many_arguments)]
#[inline]
pub(super) fn project_raw(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    file: &mut FileReplay,
    source: &SourceDef,
    start_offset: u64,
    payload: &[u8],
    report: &mut RebuildReport,
    instrument: &Instrument,
) -> bool {
    if file
        .last_offset
        .is_some_and(|previous| start_offset <= previous)
    {
        report.duplicate_records_skipped += 1;
        return false;
    }
    file.last_offset = Some(start_offset);
    file.ctx.agent_version = source.agent_version.clone();

    report.raw_records += 1;
    instrument.add(Metric::RecordsFramed, 1);
    file.ctx.src_offset = Some(start_offset as i64);
    if over_projection_limit(adapter, &file.source.stream_id, payload) {
        report.quarantined += 1;
        instrument.add(Metric::RecordsQuarantined, 1);
        file.ops.push(quarantined_record(
            &runtime.id,
            file.ctx.agent_version.as_deref(),
            &file.source.stream_id,
            file.ctx.src_cursor,
            start_offset,
        ));
    } else {
        let outcome = instrument.time(Metric::NormalizeNanos, || {
            adapter
                .ingest_line(&mut file.ctx, &file.source.stream_id, payload)
                .into_projection()
        });
        match &outcome.status {
            LineStatus::Parsed => report.parsed += 1,
            LineStatus::UnknownType => report.unknown += 1,
            LineStatus::HardError { .. } => {
                file.generation_clean = false;
                report.hard_errors += 1;
            }
        }
        file.slots.note(&outcome.ops);
        file.ops.extend(outcome.ops);
    }
    true
}
