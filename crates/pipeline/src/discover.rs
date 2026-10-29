//! Phase 1 — discovery. Probe every registered adapter's data root and
//! report what is installed. Nothing is ingested here: the app shows the
//! result and the USER decides which runtimes get `enabled = true` (the
//! consent flag on `agent_runtime`).

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rosetta_adapter_support::{AgentAdapter, GroundTruthDoc, StreamSpec};
use rosetta_core::ids;
use rosetta_core::rows::RuntimeRow;

use crate::instrument::{Instrument, Metric};
use crate::{IngestOptions, PipelineError};

#[derive(Debug)]
pub struct Discovery {
    /// Ready-to-apply row. `data_root` keeps the layout's `~/...` form so the
    /// runtime id is machine-independent; `enabled` is false until consent.
    pub runtime: RuntimeRow,
    /// The expanded local root discovery actually probed.
    pub root: PathBuf,
    pub installed: bool,
    pub file_count: usize,
    /// Content-free estimate from the adapter's session transcript streams,
    /// including child sessions. Unknown for unsupported layouts or a
    /// truncated walk; zero for a complete walk with no session files.
    pub session_count: Option<usize>,
    pub total_bytes: u64,
    /// The subset of the census the backfill will actually read: files whose
    /// matching stream has a driver. Inventory-only streams (todos, plans,
    /// config) count toward the roll call's footprint above, but never
    /// toward a progress denominator — a meter that includes them can never
    /// reach what the skim and stream phases actually finish.
    pub streamable_count: usize,
    pub streamable_bytes: u64,
    pub newest_modified_ms: i64,
    pub walk_truncated: bool,
    /// The already-filtered deterministic roster. A finite cold ingest can
    /// hand this directly to metadata admission instead of walking the same
    /// runtime root twice before it reads one transcript record.
    pub inventory: Vec<rosetta_capture::ScannedFile>,
}

/// Expand the layout's `~` against a caller-chosen home. Only the leading
/// component expands — layouts never embed absolute machine paths.
pub fn expand_data_root(data_root: &str, home: &Path) -> PathBuf {
    match data_root.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => match data_root {
            "~" => home.to_path_buf(),
            other => PathBuf::from(other),
        },
    }
}

/// Probe every adapter's layout root under `home`: one [`Discovery`] per
/// adapter, in registry order, whose runtime identity is the layout's
/// machine-independent `~` form.
pub fn discover(
    adapters: &[&dyn AgentAdapter],
    home: &Path,
    opts: &IngestOptions,
    instrument: &Instrument,
) -> Result<Vec<Discovery>, PipelineError> {
    let roots: Vec<PathBuf> = adapters
        .iter()
        .map(|adapter| expand_data_root(&adapter.layout().data_root, home))
        .collect();
    let census: Vec<CensusRoot<'_>> = adapters
        .iter()
        .zip(&roots)
        .map(|(adapter, root)| CensusRoot {
            adapter: *adapter,
            data_root: &adapter.layout().data_root,
            root,
        })
        .collect();
    discover_roots(&census, opts, instrument)
}

/// One root for [`discover_roots`]: the adapter whose layout walks it, the
/// identity string its runtime id is minted from, and the directory walked.
#[derive(Clone, Copy)]
pub struct CensusRoot<'a> {
    pub adapter: &'a dyn AgentAdapter,
    pub data_root: &'a str,
    pub root: &'a Path,
}

/// [`discover_root`] for several roots at once, one scoped thread per root.
/// Roots are separate directory trees, so their walks only overlap latency.
/// Results keep the given order, and the first root that fails, in that
/// order, fails the census with its own error: the one a serial census
/// would have stopped at.
pub fn discover_roots(
    roots: &[CensusRoot<'_>],
    opts: &IngestOptions,
    instrument: &Instrument,
) -> Result<Vec<Discovery>, PipelineError> {
    let census = |root: &CensusRoot<'_>| {
        discover_root(root.adapter, root.data_root, root.root, opts, instrument)
    };
    if roots.len() < 2 {
        return roots.iter().map(census).collect();
    }
    std::thread::scope(|scope| {
        let walks: Vec<_> = roots
            .iter()
            .map(|root| scope.spawn(move || census(root)))
            .collect();
        walks
            .into_iter()
            .map(|walk| {
                walk.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

/// The census walk of one root for one adapter. `data_root` is the identity
/// string the runtime id is minted from and `root` the directory actually
/// walked: the layout's `~` form over the expanded home for a discovered
/// runtime, or one absolute path for both when a supervisor enrolls an extra
/// root of the same kind (2026-09-12). The walk, the installed verdict, and
/// the streamable subset are the same either way.
pub fn discover_root(
    adapter: &dyn AgentAdapter,
    data_root: &str,
    root: &Path,
    opts: &IngestOptions,
    instrument: &Instrument,
) -> Result<Discovery, PipelineError> {
    let discovered_at = UNIX_EPOCH
        .elapsed()
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    let layout = adapter.layout();
    let report = instrument.time(Metric::WalkNanos, || {
        rosetta_capture::walk_classified(
            root,
            opts.max_files,
            |dir| layout.may_claim_below(dir, |_| true),
            |path| layout.matching_stream_index(path),
        )
    })?;
    instrument.add(Metric::FilesWalked, report.files.len() as u64);

    // Installed = the root exists AND holds at least one file some layout
    // stream claims; an empty shell directory is not an install.
    let mut file_count = 0;
    let mut total_bytes = 0;
    let mut streamable_count = 0;
    let mut streamable_bytes = 0;
    let mut newest_modified_ms = 0;
    let session_streams = adapter.census_session_streams();
    let mut session_count = session_streams.map(|_| 0);
    for file in &report.files {
        file_count += 1;
        total_bytes += file.size;
        newest_modified_ms = newest_modified_ms.max(file.modified_ms);
        if let Some(stream) = inventory_stream(layout, file) {
            if adapter.stream_driver(&stream.id).is_some() {
                streamable_count += 1;
                streamable_bytes += file.size;
            }
            if session_streams.is_some_and(|streams| streams.contains(&stream.id.as_str()))
                && let Some(count) = &mut session_count
            {
                *count += 1;
            }
        }
    }

    Ok(Discovery {
        runtime: RuntimeRow {
            id: ids::runtime_id(adapter.kind(), data_root),
            kind: adapter.kind().to_string(),
            data_root: data_root.to_string(),
            enabled: false,
            account: None,
            config: None,
            skills: None,
            versions_seen: None,
            discovered_at,
            label: None,
            provenance: Default::default(),
        },
        root: root.to_path_buf(),
        installed: file_count > 0,
        file_count,
        session_count: if report.truncated {
            None
        } else {
            session_count
        },
        total_bytes,
        streamable_count,
        streamable_bytes,
        newest_modified_ms,
        walk_truncated: report.truncated,
        inventory: report.files,
    })
}

/// The stream that claimed an inventory file: the index its classifying
/// walk carried, or a fresh match for an inventory walked without one.
pub(crate) fn inventory_stream<'a>(
    layout: &'a GroundTruthDoc,
    file: &rosetta_capture::ScannedFile,
) -> Option<&'a StreamSpec> {
    let stream = match file.class {
        Some(index) => layout.streams.get(index),
        None => layout.matching_stream(&file.relative_path),
    };
    debug_assert_eq!(
        stream.map(|stream| &stream.id),
        layout
            .matching_stream(&file.relative_path)
            .map(|stream| &stream.id),
        "{} carries a stream from another layout",
        file.relative_path
    );
    stream
}
