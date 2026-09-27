//! Closed sources for the streamed fresh-cold build.
//!
//! The capture drivers run unchanged against a private planning store: the
//! cursor CAS chain, generations, raw-ledger order, and restart detection are
//! the same control plane [`crate::stream_backfill_into`] drives. What changes
//! is retention. Instead of holding every normalized batch until the runtime
//! closes, the producer walks the roster in source-path order — the order the
//! sole canonical writer applies a runtime's sources in, so every
//! order-sensitive canonical upsert resolves exactly as it would for the whole
//! runtime at once — and hands each source to the writer as a closed source
//! (terminal generation, final checkpoint, validated contiguous ops) as soon as
//! nothing can supersede it. Only a JSONL transcript whose session still has an
//! uncaptured transcript is held back: a later restart of that session would
//! force-replay its siblings into replacement generations, and the writer only
//! ever sees one terminal generation per source.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rosetta_adapter_support::{AgentAdapter, StreamDriver};
use rosetta_capture::{SourceDef, Tailer};
use rosetta_core::IngestOp;
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_store::{Batch, SourceMode, Store, queries};

use crate::checkpoint::PipeCheckpoint;
use crate::instrument::{Instrument, Metric};
use crate::live::PollEvidence;
use crate::meta::SessionFile;
use crate::stream::{
    FilePass, PassMode, StreamReport, fresh_cursor, ingest_file, rotating_audit_paths,
};
use crate::{IngestOptions, PipelineError};

/// One source whose generation is terminal: exactly what
/// [`crate::prepare_captured_runtime`] computes per file, ready for the
/// canonical writer. Process-local and disposable; the raw ledger remains the
/// recovery source.
#[derive(Debug)]
pub struct ClosedSource {
    pub(crate) source: SourceDef,
    pub(crate) generation: u64,
    pub(crate) ops: Vec<IngestOp>,
    pub(crate) checkpoint: String,
    /// What the live stat poll reads from `checkpoint`, taken from the decode
    /// that validated it, so the writer can hand it on without decoding.
    pub(crate) evidence: PollEvidence,
    pub(crate) generation_clean: bool,
    pub(crate) activate: bool,
    pub(crate) payload_bytes: u64,
    /// What [`crate::StreamedPreparer`] did to the source as it was emitted;
    /// the writer applies only prepared sources.
    pub(crate) prepared: Option<crate::early::Prepared>,
}

impl ClosedSource {
    pub fn relative_path(&self) -> &str {
        &self.source.relative_path
    }

    /// Framed payload bytes this source consumed — the producer's in-flight
    /// budget unit.
    pub fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }
}

/// What the canonical writer needs before a streamed runtime's first source:
/// every roster source (its cursor row and, for a session no metadata batch
/// named, its session stub), in source-path order.
#[derive(Debug)]
pub struct StreamedSetup {
    pub(crate) sources: Vec<SourceDef>,
    pub(crate) metadata_sessions: BTreeSet<String>,
}

impl StreamedSetup {
    pub fn sources(&self) -> usize {
        self.sources.len()
    }
}

fn source_def(file: &SessionFile) -> SourceDef {
    SourceDef {
        ledger_only: file.ledger_only,
        relative_path: file.relative_path.clone(),
        stream_id: file.stream_id.clone(),
        session_id: file.session_id.clone(),
        native_session_id: file.native_session_id.clone(),
        agent_version: file.agent_version.clone(),
    }
}

/// Close the planner's metadata phase: take the metadata batches it retained
/// (the caller has already published identical rows to the canonical store)
/// and describe the runtime's sources for the writer's setup.
pub fn streamed_setup(
    files: &[SessionFile],
    planner: &mut Store,
) -> Result<StreamedSetup, PipelineError> {
    let mut metadata_sessions = BTreeSet::new();
    for batch in planner.drain_planned_batches()? {
        if batch.source.is_some() {
            return Err(PipelineError::Config(
                "cold planner captured a source before its streamed setup".to_string(),
            ));
        }
        for op in &batch.ops {
            if let IngestOp::SessionMeta(row) = op {
                metadata_sessions.insert(row.id.clone());
            }
        }
    }
    let mut sources = files.iter().map(source_def).collect::<Vec<_>>();
    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if let Some(pair) = sources
        .windows(2)
        .find(|pair| pair[0].relative_path == pair[1].relative_path)
    {
        return Err(PipelineError::Config(format!(
            "cold roster names source {} more than once",
            pair[0].relative_path
        )));
    }
    Ok(StreamedSetup {
        sources,
        metadata_sessions,
    })
}

/// Validate one source's retained planner batches exactly like
/// [`crate::prepare_captured_runtime`]: a started, terminal generation with
/// contiguous operation sequences and a decodable final checkpoint of this
/// adapter's normalizer. Batches of a superseded generation are dropped.
fn close_file(
    adapter: &dyn AgentAdapter,
    file: &SessionFile,
    cursor: queries::CursorState,
    batches: Vec<Batch>,
    payload_bytes: u64,
) -> Result<ClosedSource, PipelineError> {
    if cursor.highest_generation == 0 {
        return Err(PipelineError::Config(format!(
            "cold source {} never started a generation",
            file.relative_path
        )));
    }
    let generation = cursor.highest_generation;
    let mut ops: Vec<IngestOp> = Vec::new();
    let mut last_mode = None;
    for batch in batches {
        let source_write = batch
            .source
            .as_ref()
            .expect("closed-source batches are grouped by their source");
        if source_write.session_id != file.session_id {
            return Err(PipelineError::Config(format!(
                "cold source {} moved from session {} to {}",
                file.relative_path, file.session_id, source_write.session_id
            )));
        }
        if source_write.generation < generation {
            // A rewrite observed during capture superseded this incomplete
            // generation; the planner swept it, and so does the closed source.
            continue;
        }
        if source_write.generation > generation {
            return Err(PipelineError::Config(format!(
                "cold source {} retained generation {} above planner watermark {}",
                file.relative_path, source_write.generation, generation
            )));
        }
        let expected = u64::try_from(ops.len()).map_err(|_| {
            PipelineError::Config(format!(
                "cold source {} operation count exceeds u64",
                file.relative_path
            ))
        })?;
        if source_write.first_sequence != expected {
            return Err(PipelineError::Config(format!(
                "cold source {} has non-contiguous operation sequence {} after {}",
                file.relative_path, source_write.first_sequence, expected
            )));
        }
        last_mode = Some(source_write.mode);
        ops.extend(batch.ops);
    }
    let terminal = matches!(last_mode, Some(SourceMode::Activate | SourceMode::Abandon))
        || (cursor.active_generation == generation
            && matches!(last_mode, Some(SourceMode::Append)));
    if !terminal {
        return Err(PipelineError::Config(format!(
            "cold source {} stopped without a terminal generation",
            file.relative_path
        )));
    }
    let checkpoint = PipeCheckpoint::decode(&cursor.checkpoint).ok_or_else(|| {
        PipelineError::Config(format!(
            "cold source {} ended with an undecodable checkpoint",
            file.relative_path
        ))
    })?;
    if checkpoint.source_generation != generation {
        return Err(PipelineError::Config(format!(
            "cold source {} checkpoint generation {} differs from control watermark {}",
            file.relative_path, checkpoint.source_generation, generation
        )));
    }
    if checkpoint.normalizer_version != adapter.normalizer_version() {
        return Err(PipelineError::Config(format!(
            "cold source {} checkpoint normalizer {} differs from adapter {}",
            file.relative_path,
            checkpoint.normalizer_version,
            adapter.normalizer_version()
        )));
    }
    Ok(ClosedSource {
        source: source_def(file),
        generation,
        ops,
        evidence: PollEvidence::of(Some(&checkpoint)),
        checkpoint: cursor.checkpoint,
        generation_clean: checkpoint.generation_clean,
        activate: cursor.active_generation == generation,
        payload_bytes,
        prepared: None,
    })
}

/// Close every source of one capture unit from the batches the planner
/// retained for it. Anything retained for a source outside the unit is a
/// broken planner invariant.
fn close_unit(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    planner: &mut Store,
    members: &[&SessionFile],
    payload_bytes: u64,
    instrument: &Instrument,
) -> Result<Vec<ClosedSource>, PipelineError> {
    let started = Instant::now();
    let mut by_cursor: HashMap<i64, Vec<Batch>> = HashMap::new();
    for batch in planner.drain_planned_batches()? {
        if let Some(source) = batch.source.as_ref() {
            by_cursor.entry(source.cursor_id).or_default().push(batch);
        }
    }
    let count = u64::try_from(members.len().max(1)).unwrap_or(u64::MAX);
    let mut closed = Vec::with_capacity(members.len());
    for file in members {
        let cursor = queries::cursor_state(planner.conn(), &runtime.id, &file.relative_path)?
            .ok_or_else(|| {
                PipelineError::Config(format!(
                    "cold planner lost cursor for {}",
                    file.relative_path
                ))
            })?;
        let batches = by_cursor.remove(&cursor.id).unwrap_or_default();
        closed.push(close_file(
            adapter,
            file,
            cursor,
            batches,
            payload_bytes.div_ceil(count),
        )?);
    }
    if let Some(cursor_id) = by_cursor.keys().next() {
        return Err(PipelineError::Config(format!(
            "cold planner retained unknown cursor {cursor_id}"
        )));
    }
    instrument.add(
        Metric::ColdPrepareNanos,
        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
    );
    Ok(closed)
}

struct Slot {
    session_id: String,
    transcript: bool,
    closed: Option<ClosedSource>,
}

/// Emits closed sources in source-path order. A transcript waits while its
/// session still has an uncaptured transcript or a pending sibling heal;
/// everything behind a waiting slot waits with it, so the writer's order is
/// always the canonical path order.
struct Frontier {
    slots: Vec<Slot>,
    position: HashMap<String, usize>,
    next: usize,
    transcripts_left: HashMap<String, usize>,
    restarted: BTreeSet<String>,
}

impl Frontier {
    fn new(ordered: &[&SessionFile]) -> Self {
        let mut transcripts_left: HashMap<String, usize> = HashMap::new();
        let mut slots = Vec::with_capacity(ordered.len());
        let mut position = HashMap::with_capacity(ordered.len());
        for (index, file) in ordered.iter().enumerate() {
            let transcript = file.driver == StreamDriver::JsonLines;
            if transcript {
                *transcripts_left.entry(file.session_id.clone()).or_default() += 1;
            }
            position.insert(file.relative_path.clone(), index);
            slots.push(Slot {
                session_id: file.session_id.clone(),
                transcript,
                closed: None,
            });
        }
        Self {
            slots,
            position,
            next: 0,
            transcripts_left,
            restarted: BTreeSet::new(),
        }
    }

    fn settled(&self, slot: &Slot) -> bool {
        !slot.transcript
            || (self
                .transcripts_left
                .get(&slot.session_id)
                .is_none_or(|left| *left == 0)
                && !self.restarted.contains(&slot.session_id))
    }

    /// Record a source closed by the main pass.
    fn captured(&mut self, closed: ClosedSource) -> Result<(), PipelineError> {
        let index = self.slot_index(closed.relative_path())?;
        let slot = &mut self.slots[index];
        if slot.closed.is_some() || index < self.next {
            return Err(PipelineError::Config(format!(
                "cold source {} closed twice",
                closed.relative_path()
            )));
        }
        if slot.transcript
            && let Some(left) = self.transcripts_left.get_mut(&slot.session_id)
        {
            *left = left.saturating_sub(1);
        }
        slot.closed = Some(closed);
        Ok(())
    }

    /// Replace a held transcript with the replacement generation its sibling
    /// heal produced. Held means never emitted: the writer sees one terminal
    /// generation per source.
    fn healed(&mut self, closed: ClosedSource) -> Result<(), PipelineError> {
        let index = self.slot_index(closed.relative_path())?;
        if index < self.next || self.slots[index].closed.is_none() {
            return Err(PipelineError::Config(format!(
                "healed cold source {} was not held for its session",
                closed.relative_path()
            )));
        }
        self.slots[index].closed = Some(closed);
        Ok(())
    }

    fn slot_index(&self, path: &str) -> Result<usize, PipelineError> {
        self.position.get(path).copied().ok_or_else(|| {
            PipelineError::Config(format!("closed cold source {path} is not in the roster"))
        })
    }

    fn transcripts_left(&self, session_id: &str) -> usize {
        self.transcripts_left.get(session_id).copied().unwrap_or(0)
    }

    fn emit_ready(
        &mut self,
        emit: &mut dyn FnMut(Vec<ClosedSource>) -> Result<(), PipelineError>,
    ) -> Result<(), PipelineError> {
        let mut ready = Vec::new();
        while self.next < self.slots.len() {
            let slot = &self.slots[self.next];
            if slot.closed.is_none() || !self.settled(slot) {
                break;
            }
            ready.push(self.slots[self.next].closed.take().expect("checked above"));
            self.next += 1;
        }
        if ready.is_empty() {
            Ok(())
        } else {
            emit(ready)
        }
    }

    fn finish(
        &mut self,
        emit: &mut dyn FnMut(Vec<ClosedSource>) -> Result<(), PipelineError>,
    ) -> Result<(), PipelineError> {
        self.emit_ready(emit)?;
        if self.next < self.slots.len() {
            let path = self
                .position
                .iter()
                .find(|(_, index)| **index == self.next)
                .map(|(path, _)| path.clone())
                .unwrap_or_default();
            return Err(PipelineError::Config(format!(
                "cold source {path} never closed"
            )));
        }
        Ok(())
    }
}

fn record_outcome(
    outcome: FilePass,
    file: &SessionFile,
    report: &mut StreamReport,
    file_health: &mut BTreeMap<String, bool>,
    session_health: &mut BTreeMap<String, bool>,
) {
    let complete = outcome == FilePass::Finished(true);
    file_health.insert(file.relative_path.clone(), complete);
    session_health
        .entry(file.session_id.clone())
        .and_modify(|healthy| *healthy &= complete);
    match outcome {
        FilePass::Paused => report.files_paused += 1,
        FilePass::Finished(true) => {}
        FilePass::Finished(false) => report.files_incomplete += 1,
    }
}

fn virtual_parent(file: &SessionFile) -> Result<&str, PipelineError> {
    file.relative_path
        .split_once('#')
        .map(|(physical, _)| physical)
        .ok_or_else(|| {
            PipelineError::Config(format!(
                "snapshot source {} has no #session suffix",
                file.relative_path
            ))
        })
}

/// [`crate::stream_backfill_into`] for the streamed fresh-cold build: the
/// same capture drivers and planner control plane, walked in source-path
/// order, with each source handed to `emit` as a [`ClosedSource`] once it can
/// no longer be superseded. `emit` receives every roster source exactly once,
/// in path order. The planner must be a cold-capture planner whose metadata
/// batches were already taken by [`streamed_setup`].
///
/// Every roster source's cursor is created before the walk, in one planner
/// transaction and in path order — the order the walk would have created
/// them one at a time, so each cursor gets the rowid it always had. The
/// ledger's file id for each source is its index in that order (its roster
/// key), in every segment.
#[allow(clippy::too_many_arguments)]
pub fn stream_closed_sources_into(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    planner: &mut Store,
    files: &[SessionFile],
    opts: &IngestOptions,
    instrument: &Instrument,
    mut ledger: Option<&mut rosetta_capture::Ledger>,
    emit: &mut dyn FnMut(Vec<ClosedSource>) -> Result<(), PipelineError>,
) -> Result<StreamReport, PipelineError> {
    let ordered = path_ordered(files);
    create_roster_cursors(adapter, runtime, planner, &ordered, instrument)?;
    if let Some(ledger) = ledger.as_deref_mut() {
        ledger.set_source_keys(Arc::new(roster_keys(&ordered)?))?;
    }
    walk_closed_sources(
        adapter, runtime, root, planner, &ordered, opts, instrument, ledger, emit,
    )
}

/// Each roster source's ledger file id: its index in path order.
fn roster_keys(ordered: &[&SessionFile]) -> Result<HashMap<SourceDef, u32>, PipelineError> {
    ordered
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let key = u32::try_from(index).map_err(|_| {
                PipelineError::Config("cold roster has more sources than ledger ids".to_string())
            })?;
            Ok((source_def(file), key))
        })
        .collect()
}

fn path_ordered(files: &[SessionFile]) -> Vec<&SessionFile> {
    let mut ordered: Vec<&SessionFile> = files.iter().collect();
    ordered.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    ordered
}

/// Create the fresh cursor of every roster source that has none, in one
/// planner transaction, in path order. Cursor ids are rowids allocated in
/// insertion order, so each id equals the one creation at the source's turn
/// in the walk gives it; the batch names no source, so closing a unit
/// ignores it.
fn create_roster_cursors(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    planner: &mut Store,
    ordered: &[&SessionFile],
    instrument: &Instrument,
) -> Result<(), PipelineError> {
    let mut checkpoints = Vec::with_capacity(ordered.len());
    for file in ordered {
        if queries::cursor_state(planner.conn(), &runtime.id, &file.relative_path)?.is_none() {
            checkpoints.push(fresh_cursor(adapter, runtime, file));
        }
    }
    if !checkpoints.is_empty() {
        instrument.apply_owned(
            planner,
            Batch {
                ops: vec![],
                checkpoints,
                source: None,
            },
        )?;
    }
    Ok(())
}

/// The walk behind [`stream_closed_sources_into`], over the roster in path
/// order. A source without a cursor gets one at its turn.
#[allow(clippy::too_many_arguments)]
fn walk_closed_sources(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    planner: &mut Store,
    ordered: &[&SessionFile],
    opts: &IngestOptions,
    instrument: &Instrument,
    mut ledger: Option<&mut rosetta_capture::Ledger>,
    emit: &mut dyn FnMut(Vec<ClosedSource>) -> Result<(), PipelineError>,
) -> Result<StreamReport, PipelineError> {
    let tailer = Tailer::new(opts.tail.clone())?;
    let transcripts: Vec<&SessionFile> = ordered
        .iter()
        .copied()
        .filter(|file| file.driver == StreamDriver::JsonLines)
        .collect();
    let audit_paths =
        rotating_audit_paths(planner, &runtime.id, &transcripts, opts.audit_files_per_run)?;
    let audit_set: BTreeSet<&str> = audit_paths.iter().map(String::as_str).collect();

    let mut report = StreamReport::default();
    let mut session_health: BTreeMap<String, bool> = ordered
        .iter()
        .map(|file| (file.session_id.clone(), true))
        .collect();
    let mut file_health: BTreeMap<String, bool> = BTreeMap::new();
    let mut restarted_files: BTreeSet<String> = BTreeSet::new();
    let mut frontier = Frontier::new(ordered);

    let mut index = 0;
    while index < ordered.len() {
        opts.check_cancelled()?;
        let file = ordered[index];
        if file.driver == StreamDriver::JsonLines {
            report.files += 1;
            let restarts_before = report.generations_restarted;
            let payload_before = report.payload_bytes;
            let mode = if audit_set.contains(file.relative_path.as_str()) {
                PassMode::Audit
            } else {
                PassMode::Normal
            };
            let outcome = ingest_file(
                adapter,
                runtime,
                root,
                planner,
                file,
                &tailer,
                opts,
                instrument,
                &mut report,
                mode,
                ledger.as_deref_mut(),
            )?;
            if report.generations_restarted > restarts_before {
                restarted_files.insert(file.relative_path.clone());
                frontier.restarted.insert(file.session_id.clone());
            }
            record_outcome(
                outcome,
                file,
                &mut report,
                &mut file_health,
                &mut session_health,
            );
            let payload = report.payload_bytes - payload_before;
            for closed in close_unit(adapter, runtime, planner, &[file], payload, instrument)? {
                frontier.captured(closed)?;
            }

            // A generation restart swept and rebuilt that file's rows, but
            // merge ops a sibling contributed into them died with the old
            // rows. Once the session's last transcript is captured, force one
            // replay of its other transcripts so cross-file merges re-derive —
            // the same heal `stream_backfill_into` runs after its JSONL pass.
            if frontier.transcripts_left(&file.session_id) == 0
                && frontier.restarted.contains(&file.session_id)
            {
                for sibling in transcripts.iter().copied().filter(|sibling| {
                    sibling.session_id == file.session_id
                        && !restarted_files.contains(&sibling.relative_path)
                }) {
                    opts.check_cancelled()?;
                    report.siblings_healed += 1;
                    let payload_before = report.payload_bytes;
                    let outcome = ingest_file(
                        adapter,
                        runtime,
                        root,
                        planner,
                        sibling,
                        &tailer,
                        opts,
                        instrument,
                        &mut report,
                        PassMode::ForceHeal,
                        ledger.as_deref_mut(),
                    )?;
                    record_outcome(
                        outcome,
                        sibling,
                        &mut report,
                        &mut file_health,
                        &mut session_health,
                    );
                    let payload = report.payload_bytes - payload_before;
                    for closed in
                        close_unit(adapter, runtime, planner, &[sibling], payload, instrument)?
                    {
                        frontier.healed(closed)?;
                    }
                }
                frontier.restarted.remove(&file.session_id);
            }
            index += 1;
        } else {
            // One physical document or database: its virtual members are
            // contiguous in path order and read from one snapshot.
            let parent = virtual_parent(file)?;
            let mut end = index + 1;
            while end < ordered.len()
                && ordered[end].driver == file.driver
                && virtual_parent(ordered[end])? == parent
            {
                end += 1;
            }
            let group = &ordered[index..end];
            let payload_before = report.payload_bytes;
            if file.driver == StreamDriver::DocumentSnapshot {
                crate::document::stream_document_group(
                    adapter,
                    runtime,
                    root,
                    planner,
                    parent,
                    group,
                    opts,
                    instrument,
                    &mut ledger,
                    &mut report,
                    &mut file_health,
                    &mut session_health,
                )?;
            } else {
                crate::snapshot::stream_snapshot_group(
                    adapter,
                    runtime,
                    root,
                    planner,
                    parent,
                    group,
                    opts,
                    instrument,
                    &mut ledger,
                    &mut report,
                    &mut file_health,
                    &mut session_health,
                )?;
            }
            let payload = report.payload_bytes - payload_before;
            for closed in close_unit(adapter, runtime, planner, group, payload, instrument)? {
                frontier.captured(closed)?;
            }
            index = end;
        }
        frontier.emit_ready(emit)?;
    }
    frontier.finish(emit)?;

    let mut last_completed_audit = None;
    for path in &audit_paths {
        if file_health.get(path).copied() != Some(true) {
            break;
        }
        last_completed_audit = Some(path.as_str());
        report.files_audited += 1;
    }
    if let Some(path) = last_completed_audit {
        planner.set_audit_path(&runtime.id, path)?;
    }
    if let Some(progress) = &opts.progress {
        progress.reading_finished();
    }
    // The planner's own readiness badges, kept for the report's accounting;
    // canonical readiness is the writer's admission flip.
    for (session_id, complete) in session_health {
        let next = if complete {
            StreamState::Indexed
        } else {
            StreamState::Streaming
        };
        if planner.set_session_stream_state(&session_id, next)? && complete {
            report.sessions_indexed += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
