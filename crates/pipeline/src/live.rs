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

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{Ledger, SourceEvent, SourceEvents};
use rosetta_core::rows::RuntimeRow;
use rosetta_store::Store;

use crate::instrument::Instrument;
use crate::meta::{SessionFile, meta_ingest_into};
use crate::stream::{StreamReport, merge_stats, stream_backfill_into};
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
        Ok(Self {
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
        })
    }

    pub fn report(&self) -> &LiveReport {
        &self.report
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
        self.report.ticks += 1;
        let mut changed: BTreeSet<String> = BTreeSet::new();
        let mut needs_admission = false;
        let mut poll_due = self.last_poll.elapsed() >= self.live.poll_interval;

        let mut budget = self.live.max_coalesced;
        let mut wait = self.live.tick;
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
                    let children: Vec<String> = self
                        .files
                        .iter()
                        .filter(|file| {
                            file.relative_path == path
                                || crate::snapshot::virtual_db_path(&file.relative_path)
                                    == Some(path.as_str())
                        })
                        .map(|file| file.relative_path.clone())
                        .collect();
                    if !children.is_empty() {
                        changed.extend(children);
                    } else if self.claimed(&path) {
                        needs_admission = true;
                    } else {
                        self.report.hints_ignored += 1;
                    }
                }
                SourceEvent::Rescan => {
                    self.report.rescans += 1;
                    poll_due = true;
                }
            }
        }

        if poll_due {
            self.poll(store, &mut changed, &mut needs_admission)?;
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
            merge_stream(&mut self.report.stream, outcome);
        }

        // The rotating audit rides poll ticks and sweeps ALL known files —
        // its duty cycle is poll_interval × (files ÷ audit_files_per_run).
        // The picks come from the store's own rotation state, so restarting
        // the process never restarts the sweep.
        if poll_due && self.opts.audit_files_per_run > 0 {
            let audit_candidates: Vec<&SessionFile> = self
                .files
                .iter()
                .filter(|file| file.driver != rosetta_adapter_support::StreamDriver::SqliteSnapshot)
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
                merge_stream(&mut self.report.stream, outcome);
            }
        }
        Ok(())
    }

    /// Does any layout stream with a driver claim this path?
    fn claimed(&self, relative_path: &str) -> bool {
        self.adapter
            .layout()
            .matching_stream(relative_path)
            .is_some_and(|stream| self.adapter.stream_driver(&stream.id).is_some())
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
    ) -> Result<(), PipelineError> {
        self.report.polls += 1;
        self.last_poll = Instant::now();
        let layout = self.adapter.layout();
        let walk = rosetta_capture::walk_filtered(&self.root, self.opts.max_files, |path| {
            layout.matching_stream(path).is_some()
        })?;
        for walked in &walk.files {
            match self
                .files
                .iter_mut()
                .find(|file| file.relative_path == walked.relative_path)
            {
                Some(known) => {
                    let ingested_len = rosetta_store::queries::cursor_state(
                        store.conn(),
                        &self.runtime.id,
                        &walked.relative_path,
                    )?
                    .and_then(|cursor| {
                        crate::checkpoint::PipeCheckpoint::decode(&cursor.checkpoint)
                    })
                    .and_then(|checkpoint| checkpoint.tail_checkpoint())
                    .map(|tail| tail.observed_len);
                    let behind_cursor = ingested_len != Some(walked.size);
                    let touched = known.modified_ms != walked.modified_ms;
                    if behind_cursor || touched {
                        known.size = walked.size;
                        known.modified_ms = walked.modified_ms;
                        changed.insert(known.relative_path.clone());
                    }
                }
                None => {
                    // A database file never appears in `files` itself — its
                    // sessions do, as virtual sources. Nominate them and let
                    // the snapshot pass's stamp check decide cheaply.
                    let children: Vec<String> = self
                        .files
                        .iter()
                        .filter(|file| {
                            crate::snapshot::virtual_db_path(&file.relative_path)
                                == Some(walked.relative_path.as_str())
                        })
                        .map(|file| file.relative_path.clone())
                        .collect();
                    if !children.is_empty() {
                        changed.extend(children);
                    } else if self.claimed(&walked.relative_path) {
                        *needs_admission = true;
                    }
                }
            }
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

fn merge_stream(total: &mut StreamReport, tick: StreamReport) {
    total.files += tick.files;
    total.files_missing += tick.files_missing;
    total.batches += tick.batches;
    total.records += tick.records;
    total.oversized += tick.oversized;
    total.parsed += tick.parsed;
    total.unknown += tick.unknown;
    total.hard_errors += tick.hard_errors;
    total.generations_restarted += tick.generations_restarted;
    total.files_audited += tick.files_audited;
    total.conflicts += tick.conflicts;
    total.retries_exhausted += tick.retries_exhausted;
    total.files_incomplete += tick.files_incomplete;
    total.sessions_indexed += tick.sessions_indexed;
    merge_stats(&mut total.stats, &tick.stats);
}
