//! Phase 1 — discovery. Probe every registered adapter's data root and
//! report what is installed. Nothing is ingested here: the app shows the
//! result and the USER decides which runtimes get `enabled = true` (the
//! consent flag on `agent_runtime`).

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rosetta_adapter_support::AgentAdapter;
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

pub fn discover(
    adapters: &[&dyn AgentAdapter],
    home: &Path,
    opts: &IngestOptions,
    instrument: &Instrument,
) -> Result<Vec<Discovery>, PipelineError> {
    let discovered_at = UNIX_EPOCH
        .elapsed()
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    let mut out = Vec::with_capacity(adapters.len());
    for adapter in adapters {
        let layout = adapter.layout();
        let root = expand_data_root(&layout.data_root, home);
        let report = instrument.time(Metric::WalkNanos, || {
            rosetta_capture::walk_filtered(&root, opts.max_files, |path| {
                layout.matching_stream(path).is_some()
            })
        })?;
        instrument.add(Metric::FilesWalked, report.files.len() as u64);

        // Installed = the root exists AND holds at least one file some layout
        // stream claims; an empty shell directory is not an install.
        let mut file_count = 0;
        let mut total_bytes = 0;
        let mut streamable_count = 0;
        let mut streamable_bytes = 0;
        let mut newest_modified_ms = 0;
        for file in &report.files {
            file_count += 1;
            total_bytes += file.size;
            newest_modified_ms = newest_modified_ms.max(file.modified_ms);
            let driven = layout
                .matching_stream(&file.relative_path)
                .is_some_and(|stream| adapter.stream_driver(&stream.id).is_some());
            if driven {
                streamable_count += 1;
                streamable_bytes += file.size;
            }
        }

        out.push(Discovery {
            runtime: RuntimeRow {
                id: ids::runtime_id(adapter.kind(), &layout.data_root),
                kind: adapter.kind().to_string(),
                data_root: layout.data_root.clone(),
                enabled: false,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at,
            },
            root,
            installed: file_count > 0,
            file_count,
            total_bytes,
            streamable_count,
            streamable_bytes,
            newest_modified_ms,
            walk_truncated: report.truncated,
            inventory: report.files,
        });
    }
    Ok(out)
}
