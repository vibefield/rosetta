//! The live lane: keep the projection current while agents are running.
//!
//! Deliberately built ON TOP of the two existing phases instead of beside
//! them — a change hint re-runs `meta_ingest` (idempotent, state-preserving)
//! when a NEW file appears, and `stream_backfill` over just the changed
//! subset otherwise. Growing the topology must never change semantics: every
//! generation, audit, health, and crash rule the batch phases enforce applies
//! verbatim to live ticks, because live ticks ARE those phases.
//!
//! Recovery rules (capture::watch): events are hints that nominate paths;
//! the tailer's checkpoint decides what actually happened. A quiet or lying
//! watcher degrades to the stat poll, never to wrong data.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{Ledger, SourceEvent, SourceEvents};
use rosetta_core::rows::RuntimeRow;
use rosetta_store::Store;

use crate::instrument::{Instrument, Metric};
use crate::meta::{SessionFile, meta_ingest_into};
use crate::stream::{StreamReport, stream_backfill_into};
use crate::{IngestOptions, PipelineError};

#[derive(Debug, Clone)]
pub struct LiveOptions {
    /// How long one tick waits for a first event before going quiet.
    pub tick: Duration,
    /// Stat-scan fallback cadence — the watcher is never trusted to be the
    /// only signal. Poll passes also carry the rotating audit lane.
    pub poll_interval: Duration,
    /// Bounded coalescing: at most this many further events drain per tick.
    pub max_coalesced: usize,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            tick: Duration::from_millis(250),
            poll_interval: Duration::from_secs(30),
            max_coalesced: 256,
        }
    }
}

#[derive(Debug, Default)]
pub struct LiveReport {
    pub ticks: u64,
    pub events: u64,
    /// Hints for paths no layout stream claims — watcher noise, dropped.
    pub hints_ignored: u64,
    pub rescans: u64,
    pub polls: u64,
    /// Meta re-runs triggered by a previously unknown file appearing.
    pub admissions: u64,
    pub stream: StreamReport,
}

/// One watcher hint mapped against an admitted roster — the single truth
/// table shared by the live runner's ticks and the cold scheduler's
/// mid-backfill drain, so the two lanes can never disagree about what a
/// path means.
#[derive(Debug, Default)]
pub struct HintMapping {
    /// Relative paths of admitted sources this hint nominates for streaming
    /// (the path itself, or a snapshot's virtual members).
    pub nominated: Vec<String>,
    /// Bounded meta must re-admit before streaming settles this: a claimed
    /// path no roster entry covers appeared, or a snapshot-backed physical
    /// source changed and its membership may have moved.
    pub needs_admission: bool,
}

/// Map one changed-path hint against the roster. Hints are nominations, not
/// facts: the tailer's checkpoint (or the snapshot revision) decides what
/// actually happened.
pub fn map_hint(
    adapter: &dyn AgentAdapter,
    files: &[SessionFile],
    relative_path: &str,
) -> HintMapping {
    // `-shm` is SQLite's shared-memory index, written by every reader — the
    // daemon's own snapshot open included — and says nothing about content.
    // Admitting it as a hint fed the watcher the daemon's own footsteps:
    // each snapshot pass touched `-shm`, which nominated the database again
    // and re-ran bounded meta, which opened the database again, forever —
    // starving every other cold source behind the live lane.
    if relative_path.ends_with("-shm") {
        return HintMapping {
            nominated: Vec::new(),
            needs_admission: false,
        };
    }
    let snapshot = snapshot_alias(adapter, relative_path);
    let hinted = snapshot.as_deref().unwrap_or(relative_path);
    let descendant_prefix = format!("{hinted}/");
    let nominated: Vec<String> = files
        .iter()
        .filter(|file| {
            file.relative_path == hinted
                // macOS FSEvents commonly reports only the containing
                // directory. An ancestor hint nominates its admitted children;
                // their cursor anchors still decide whether any source work is
                // actually needed.
                || file.relative_path.starts_with(&descendant_prefix)
                || physical_snapshot_path(file) == Some(hinted)
        })
        .map(|file| file.relative_path.clone())
        .collect();
    if !nominated.is_empty() {
        return HintMapping {
            needs_admission: snapshot.is_some(),
            nominated,
        };
    }
    HintMapping {
        nominated,
        needs_admission: hint_claimed(adapter, relative_path),
    }
}

/// Does any layout stream with a driver claim this path?
fn hint_claimed(adapter: &dyn AgentAdapter, relative_path: &str) -> bool {
    adapter
        .layout()
        .matching_stream(relative_path)
        .is_some_and(|stream| adapter.stream_driver(&stream.id).is_some())
}

/// Normalize a physical document/database hint to the source path whose
/// virtual membership and content must be reconciled. `-shm` never reaches
/// here — [`map_hint`] drops it before aliasing.
fn snapshot_alias(adapter: &dyn AgentAdapter, relative_path: &str) -> Option<String> {
    let base = ["-wal", "-journal"]
        .iter()
        .find_map(|suffix| relative_path.strip_suffix(suffix))
        .unwrap_or(relative_path);
    let stream = adapter
        .layout()
        .matching_stream(relative_path)
        .or_else(|| adapter.layout().matching_stream(base))?;
    match adapter.stream_driver(&stream.id)? {
        rosetta_adapter_support::StreamDriver::SqliteSnapshot => Some(base.to_string()),
        rosetta_adapter_support::StreamDriver::DocumentSnapshot => Some(relative_path.to_string()),
        rosetta_adapter_support::StreamDriver::JsonLines => None,
    }
}

/// One runtime's live loop, driven a tick at a time so a caller can
/// round-robin several runtimes over one store without threads.
pub struct LiveRunner<'a> {
    adapter: &'a dyn AgentAdapter,
    runtime: RuntimeRow,
    root: PathBuf,
    events: Box<dyn SourceEvents>,
    opts: IngestOptions,
    live: LiveOptions,
    files: Vec<SessionFile>,
    ledger: Option<Ledger>,
    last_poll: Instant,
    report: LiveReport,
    /// Physical paths that back virtual sources (databases, documents), by
    /// the (size, modified) the last poll saw. A physical file's PRESENCE is
    /// not admission evidence — meta already settled its membership — so the
    /// poll admits and nominates it only when these stats move.
    physical_seen: BTreeMap<String, (u64, i64)>,
}

impl<'a> LiveRunner<'a> {
    /// Index the root and become tickable. Watch-before-scan: construct the
    /// event source BEFORE calling this, so a write landing during the
    /// initial scan still produces a hint afterwards.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        adapter: &'a dyn AgentAdapter,
        runtime: RuntimeRow,
        root: PathBuf,
        events: Box<dyn SourceEvents>,
        store: &mut Store,
        opts: IngestOptions,
        live: LiveOptions,
        instrument: &Instrument,
    ) -> Result<Self, PipelineError> {
        Self::start_with_ledger(
            adapter, runtime, root, events, store, opts, live, None, instrument,
        )
    }

    /// [`LiveRunner::start`] with a raw ledger the runner owns: every meta
    /// batch and framed record from live ticks lands there first.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_ledger(
        adapter: &'a dyn AgentAdapter,
        runtime: RuntimeRow,
        root: PathBuf,
        events: Box<dyn SourceEvents>,
        store: &mut Store,
        opts: IngestOptions,
        live: LiveOptions,
        mut ledger: Option<Ledger>,
        instrument: &Instrument,
    ) -> Result<Self, PipelineError> {
        let (_, files) = meta_ingest_into(
            adapter,
            &runtime,
            &root,
            store,
            &opts,
            instrument,
            ledger.as_mut(),
        )?;
        Ok(Self::resume_from_roster(
            adapter, runtime, root, events, files, opts, live, ledger,
        ))
    }

    /// Enter the live lane from a roster skimmed by the immediately preceding
    /// backfill. The watcher must already be armed: queued hints cover changes
    /// since that skim, and the ordinary stat poll remains the recovery lane.
    /// This avoids re-reading every metadata source at the cold/live boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn resume_from_roster(
        adapter: &'a dyn AgentAdapter,
        runtime: RuntimeRow,
        root: PathBuf,
        events: Box<dyn SourceEvents>,
        files: Vec<SessionFile>,
        opts: IngestOptions,
        live: LiveOptions,
        ledger: Option<Ledger>,
    ) -> Self {
        Self {
            adapter,
            runtime,
            root,
            events,
            opts,
            live,
            files,
            ledger,
            last_poll: Instant::now(),
            report: LiveReport::default(),
            physical_seen: BTreeMap::new(),
        }
    }

    pub fn report(&self) -> &LiveReport {
        &self.report
    }

    /// Immutable ledger segments eligible for concurrent container maintenance.
    /// The owner must retain its projection lock while using this snapshot.
    pub fn closed_ledger_segments(&self) -> Result<Vec<PathBuf>, PipelineError> {
        self.ledger.as_ref().map_or_else(
            || Ok(Vec::new()),
            |ledger| ledger.closed_segment_paths().map_err(PipelineError::from),
        )
    }

    pub fn into_report(self) -> LiveReport {
        self.report
    }

    /// One tick: wait for hints, coalesce, fall back to the stat poll when
    /// due, admit new files via meta, and stream exactly the changed subset.
    pub fn tick(
        &mut self,
        store: &mut Store,
        instrument: &Instrument,
    ) -> Result<(), PipelineError> {
        self.tick_inner(store, instrument, self.live.tick, false, true)
            .map(|_| ())
    }

    /// Drain ready hints and run due recovery work without waiting on this
    /// runtime. A shared writer waits once on `rosetta_capture::SourceWake`
    /// between passes, and publishes committed effects after each runner.
    /// Returns true when the coalescing budget was exhausted: more hints may
    /// remain, so make another fair pass before waiting again.
    pub fn tick_ready(
        &mut self,
        store: &mut Store,
        instrument: &Instrument,
    ) -> Result<bool, PipelineError> {
        self.tick_inner(store, instrument, Duration::ZERO, false, true)
    }

    /// Perform the process-start handoff without sleeping: drain every queued
    /// watcher hint and force the stat-poll recovery lane once. This closes the
    /// gap between the preceding bounded skim/backfill and the ordinary live
    /// cadence, including writes that landed while the daemon was offline or
    /// while a private cold projection was being published. Rotating audits
    /// remain on normal poll ticks; startup catch-up reads only files with
    /// changed cursor-relative evidence.
    pub fn catch_up(
        &mut self,
        store: &mut Store,
        instrument: &Instrument,
    ) -> Result<(), PipelineError> {
        self.tick_inner(store, instrument, Duration::ZERO, true, false)
            .map(|_| ())
    }

    fn tick_inner(
        &mut self,
        store: &mut Store,
        instrument: &Instrument,
        initial_wait: Duration,
        force_poll: bool,
        run_audit: bool,
    ) -> Result<bool, PipelineError> {
        self.report.ticks += 1;
        let mut changed: BTreeSet<String> = BTreeSet::new();
        let mut needs_admission = false;
        let mut poll_due = force_poll || self.last_poll.elapsed() >= self.live.poll_interval;

        let mut budget = self.live.max_coalesced;
        let mut wait = initial_wait;
        while budget > 0 {
            let Some(event) = self.events.next_event(wait) else {
                break;
            };
            // Only the first receive blocks; the rest is a bounded drain.
            wait = Duration::ZERO;
            budget -= 1;
            self.report.events += 1;
            match event {
                SourceEvent::Changed(path) => {
                    let mapping = map_hint(self.adapter, &self.files, &path);
                    if mapping.nominated.is_empty() && !mapping.needs_admission {
                        self.report.hints_ignored += 1;
                    }
                    changed.extend(mapping.nominated);
                    needs_admission |= mapping.needs_admission;
                }
                SourceEvent::Rescan => {
                    self.report.rescans += 1;
                    poll_due = true;
                }
            }
        }

        if poll_due {
            instrument.time(Metric::LivePollNanos, || {
                self.poll(store, &mut changed, &mut needs_admission, instrument)
            })?;
        }

        if needs_admission {
            self.admit(store, &mut changed, instrument)?;
        } else if !changed.is_empty() {
            self.restat(&changed);
        }

        if !changed.is_empty() {
            let subset: Vec<SessionFile> = self
                .files
                .iter()
                .filter(|file| changed.contains(&file.relative_path))
                .cloned()
                .collect();
            // Catch-up never audits: forcing a replay of exactly the files
            // that are actively growing would be the worst possible pick.
            let mut opts = self.opts.clone();
            opts.audit_files_per_run = 0;
            let outcome = stream_backfill_into(
                self.adapter,
                &self.runtime,
                &self.root,
                store,
                &subset,
                &opts,
                instrument,
                self.ledger.as_mut(),
            )?;
            self.report.stream.merge_from(&outcome);
        }

        // The rotating audit rides poll ticks and sweeps ALL known files —
        // its duty cycle is poll_interval × (files ÷ audit_files_per_run).
        // The picks come from the store's own rotation state, so restarting
        // the process never restarts the sweep.
        if run_audit && poll_due && self.opts.audit_files_per_run > 0 {
            let audit_candidates: Vec<&SessionFile> = self
                .files
                .iter()
                .filter(|file| file.driver == rosetta_adapter_support::StreamDriver::JsonLines)
                .collect();
            let picks = crate::stream::rotating_audit_paths(
                store,
                &self.runtime.id,
                &audit_candidates,
                self.opts.audit_files_per_run,
            )?;
            let audit_subset: Vec<SessionFile> = self
                .files
                .iter()
                .filter(|file| picks.contains(&file.relative_path))
                .cloned()
                .collect();
            if !audit_subset.is_empty() {
                let outcome = stream_backfill_into(
                    self.adapter,
                    &self.runtime,
                    &self.root,
                    store,
                    &audit_subset,
                    &self.opts,
                    instrument,
                    self.ledger.as_mut(),
                )?;
                self.report.stream.merge_from(&outcome);
            }
        }
        Ok(budget == 0 && self.live.max_coalesced > 0)
    }

    /// Stat scan: the fallback signal, and it must be self-healing — it
    /// compares each file against its CURSOR's observed length, not against
    /// whatever this process last saw. Any growth the watcher missed (before
    /// this process even started, or during a dropped event) is caught here;
    /// a touched mtime at the same size is offered to the stream too, where
    /// the checkpoint anchor decides. Same-size same-mtime rewrites remain
    /// the rotating audit's job, which rides along on poll passes.
    fn poll(
        &mut self,
        store: &mut Store,
        changed: &mut BTreeSet<String>,
        needs_admission: &mut bool,
        instrument: &Instrument,
    ) -> Result<(), PipelineError> {
        self.report.polls += 1;
        self.last_poll = Instant::now();
        let layout = self.adapter.layout();
        let walk = instrument.time(Metric::LivePollWalkNanos, || {
            rosetta_capture::walk_filtered(&self.root, self.opts.max_files, |path| {
                // Inventory-only streams cannot nominate a source or trigger
                // admission. Keep them out of both the stat work and file cap.
                layout
                    .matching_stream(path)
                    .is_some_and(|stream| self.adapter.stream_driver(&stream.id).is_some())
            })
        })?;
        instrument.add(Metric::LivePollFiles, walk.files.len() as u64);
        // Index once per poll instead of scanning the whole admitted roster
        // for every physical file. Preserve the old first-match behavior if
        // a roster ever contains duplicate paths.
        let mut by_path = BTreeMap::new();
        for (index, file) in self.files.iter().enumerate() {
            by_path.entry(file.relative_path.clone()).or_insert(index);
        }
        for walked in &walk.files {
            match by_path.get(&walked.relative_path).copied() {
                Some(index) => {
                    let known = &mut self.files[index];
                    let checkpoint = instrument.time(Metric::LivePollCheckpointNanos, || {
                        rosetta_store::queries::cursor_state(
                            store.conn(),
                            &self.runtime.id,
                            &walked.relative_path,
                        )
                        .map(|cursor| {
                            cursor.and_then(|cursor| {
                                crate::checkpoint::PipeCheckpoint::decode(&cursor.checkpoint)
                            })
                        })
                    })?;
                    // An adapter semantic change must replay quiet files too.
                    // Otherwise an installed projection would retain the old
                    // interpretation until this source happened to grow or
                    // the rotating audit eventually reached it.
                    let normalizer_stale = checkpoint.as_ref().is_some_and(|checkpoint| {
                        checkpoint.normalizer_version != self.adapter.normalizer_version()
                    });
                    let ingested_len = checkpoint
                        .and_then(|checkpoint| checkpoint.tail_checkpoint())
                        .map(|tail| tail.observed_len);
                    let behind_cursor = ingested_len != Some(walked.size);
                    let touched = known.modified_ms != walked.modified_ms;
                    if normalizer_stale || behind_cursor || touched {
                        known.size = walked.size;
                        known.modified_ms = walked.modified_ms;
                        changed.insert(known.relative_path.clone());
                    }
                }
                None => {
                    // A path outside the roster: a database or document
                    // backing virtual sources, a file meta judged and
                    // withheld (unidentified), or a genuinely new claim the
                    // watcher missed. PRESENCE is not evidence for any of
                    // them — treating it as such re-ran bounded meta and
                    // ledgered a full meta batch on every poll, forever, on
                    // an idle machine. Evidence is a FIRST sighting (the
                    // missed-creation recovery lane — once) or MOVEMENT
                    // since the last poll (content changed: re-check
                    // membership, retry identification).
                    let stats = (walked.size, walked.modified_ms);
                    let seen = self.physical_seen.get(&walked.relative_path);
                    let moved = seen.is_some_and(|seen| *seen != stats);
                    let first = seen.is_none();
                    self.physical_seen
                        .insert(walked.relative_path.clone(), stats);
                    if !(first || moved) {
                        continue;
                    }
                    let mapping = map_hint(self.adapter, &self.files, &walked.relative_path);
                    if moved {
                        changed.extend(mapping.nominated);
                        *needs_admission |= mapping.needs_admission;
                    } else if mapping.nominated.is_empty() {
                        // First sighting of a path meta yielded no source
                        // for: the one-shot recovery admission. A first
                        // sighting with KNOWN membership seeds silently —
                        // meta ran strictly before this runner existed.
                        *needs_admission |= mapping.needs_admission;
                    }
                }
            }
        }
        let present: BTreeSet<&str> = walk
            .files
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect();
        let missing_snapshots: BTreeSet<String> = self
            .files
            .iter()
            .filter_map(physical_snapshot_path)
            .filter(|path| !present.contains(path))
            .map(str::to_owned)
            .collect();
        for physical in missing_snapshots {
            changed.extend(
                self.files
                    .iter()
                    .filter(|file| physical_snapshot_path(file) == Some(physical.as_str()))
                    .map(|file| file.relative_path.clone()),
            );
            *needs_admission = true;
        }
        Ok(())
    }

    /// A previously unknown claimed file appeared: re-run the meta phase (it
    /// is idempotent and preserves stream state) and stream whatever is new.
    fn admit(
        &mut self,
        store: &mut Store,
        changed: &mut BTreeSet<String>,
        instrument: &Instrument,
    ) -> Result<(), PipelineError> {
        self.report.admissions += 1;
        let (_, fresh) = meta_ingest_into(
            self.adapter,
            &self.runtime,
            &self.root,
            store,
            &self.opts,
            instrument,
            self.ledger.as_mut(),
        )?;
        for file in &fresh {
            let known = self
                .files
                .iter()
                .any(|old| old.relative_path == file.relative_path);
            if !known {
                changed.insert(file.relative_path.clone());
            }
        }
        self.files = fresh;
        Ok(())
    }

    /// Refresh the stats of hinted files so cursor timestamps and queue order
    /// reflect the present, and deletions surface as Missing in the tailer.
    fn restat(&mut self, changed: &BTreeSet<String>) {
        for file in &mut self.files {
            if !changed.contains(&file.relative_path) {
                continue;
            }
            if let Ok(metadata) = std::fs::metadata(self.root.join(&file.relative_path)) {
                file.size = metadata.len();
                file.modified_ms = rosetta_capture::discovery::modified_ms(&metadata);
            }
        }
    }
}

fn physical_snapshot_path(file: &SessionFile) -> Option<&str> {
    match file.driver {
        rosetta_adapter_support::StreamDriver::DocumentSnapshot => {
            crate::document::virtual_document_path(&file.relative_path)
        }
        rosetta_adapter_support::StreamDriver::SqliteSnapshot => {
            crate::snapshot::virtual_db_path(&file.relative_path)
        }
        rosetta_adapter_support::StreamDriver::JsonLines => None,
    }
}

/// Run one runtime's live loop for a bounded number of ticks (tests, smoke
/// runs) or forever (`None`) until the process ends — the crash protocol
/// makes a hard kill safe by construction.
#[allow(clippy::too_many_arguments)]
pub fn stream_live(
    adapter: &dyn AgentAdapter,
    runtime: RuntimeRow,
    root: PathBuf,
    events: Box<dyn SourceEvents>,
    store: &mut Store,
    opts: IngestOptions,
    live: LiveOptions,
    max_ticks: Option<u64>,
    instrument: &Instrument,
) -> Result<LiveReport, PipelineError> {
    let mut runner = LiveRunner::start(
        adapter, runtime, root, events, store, opts, live, instrument,
    )?;
    let mut remaining = max_ticks;
    loop {
        match &mut remaining {
            Some(0) => break,
            Some(count) => *count -= 1,
            None => {}
        }
        runner.tick(store, instrument)?;
    }
    Ok(runner.into_report())
}
