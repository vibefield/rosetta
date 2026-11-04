//! Fresh serve's pipeline steps, driven in process: discovery, the metadata
//! plan published to the canonical store, then closed-source capture applied
//! by a streamed runtime — or the retained reference path. Recorded fixture
//! roots of every adapter are copied into temporary homes; nothing here
//! reads a live home directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rosetta_adapter_antigravity::AntigravityAdapter;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_grok::GrokAdapter;
use rosetta_adapter_hermes::HermesAdapter;
use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_opencode::OpenCodeAdapter;
use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_core::rows::RuntimeRow;
use rosetta_pipeline::{
    BlockingEmit, ClosedSource, ColdWalkOptions, EarlyToolReport, IngestOptions, Instrument,
    PollEvidence, PreparedAdmission, SessionFile, StreamedPreparer, StreamedRuntime,
    apply_prepared_runtime, discover, expand_data_root, meta_ingest_inventory_into,
    prepare_captured_runtime, stream_backfill, stream_closed_sources_into, streamed_setup,
};
use rosetta_store::Store;
use tempfile::TempDir;

/// Every registered adapter.
pub fn adapters() -> Vec<&'static dyn AgentAdapter> {
    vec![
        &AntigravityAdapter,
        &ClaudeCodeAdapter,
        &CodexAdapter,
        &CopilotAdapter,
        &GrokAdapter,
        &HermesAdapter,
        &KimiCodeAdapter,
        &OpenCodeAdapter,
        &PiAdapter,
    ]
}

pub fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    let mut entries = std::fs::read_dir(from)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &destination);
        } else {
            std::fs::copy(source, destination).unwrap();
        }
    }
}

/// Every recorded fixture scenario of one adapter: `(name, data root)`.
pub fn fixture_roots(kind: &str) -> Vec<(String, PathBuf)> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth")
        .join(kind)
        .join("fixtures");
    let mut roots = Vec::new();
    for version in std::fs::read_dir(&fixtures).unwrap() {
        let version = version.unwrap().path();
        for scenario in std::fs::read_dir(&version).unwrap() {
            let scenario = scenario.unwrap().path();
            let data = std::fs::read_dir(&scenario)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.is_dir())
                .unwrap_or_else(|| panic!("{} has no data root", scenario.display()));
            roots.push((
                scenario.file_name().unwrap().to_string_lossy().into_owned(),
                data,
            ));
        }
    }
    roots.sort();
    roots
}

/// A home holding one fixture's data root where the adapter looks for it.
pub fn home_with(adapter: &dyn AgentAdapter, data: &Path) -> TempDir {
    let home = TempDir::new().unwrap();
    copy_tree(
        data,
        &expand_data_root(&adapter.layout().data_root, home.path()),
    );
    home
}

/// Every file under `root`, sorted.
pub fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// One runtime planned as fresh serve plans it, before capture.
pub struct Planned {
    pub adapter: &'static dyn AgentAdapter,
    pub runtime: RuntimeRow,
    pub root: PathBuf,
    pub planner: Store,
    pub files: Vec<SessionFile>,
    pub opts: IngestOptions,
}

/// Discover, open a cold planner, skim metadata, and publish the metadata to
/// the canonical store — fresh serve's steps before capture.
pub fn plan(adapter: &'static dyn AgentAdapter, home: &Path, canonical: &mut Store) -> Planned {
    plan_with(adapter, home, canonical, IngestOptions::default())
}

pub fn plan_with(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    canonical: &mut Store,
    opts: IngestOptions,
) -> Planned {
    plan_into(adapter, home, canonical, opts, None)
}

/// [`plan_with`], framing the metadata into `ledger` too.
pub fn plan_into(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    canonical: &mut Store,
    opts: IngestOptions,
    ledger: Option<&mut rosetta_capture::Ledger>,
) -> Planned {
    let instrument = Instrument::new();
    let discovery = discover(&[adapter], home, &opts, &instrument)
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let (_, files) = meta_ingest_inventory_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut planner,
        &discovery.inventory,
        discovery.walk_truncated,
        &opts,
        &instrument,
        ledger,
    )
    .unwrap();
    for batch in planner.planned_metadata_batches().unwrap() {
        canonical.apply(&batch).unwrap();
    }
    Planned {
        adapter,
        runtime,
        root: discovery.root,
        planner,
        files,
        opts,
    }
}

/// A private cold store whose ordinals the streamed preparer ranks.
pub fn cold_store() -> Store {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    store
}

/// A named shared-cache RAM generation, as fresh serve opens it, and its URI.
pub fn shared_store(dir: &Path, name: &str) -> (Store, String) {
    let uri = format!(
        "file:rosetta-{name}-{}-{}?mode=memory&cache=shared",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut store =
        Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &dir.join("projection.sqlite"))
            .unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    (store, uri)
}

/// The retained reference: capture everything, prepare, apply, fold.
pub fn retained(planned: Planned, canonical: &mut Store) {
    let Planned {
        adapter,
        runtime,
        root,
        mut planner,
        files,
        opts,
    } = planned;
    let instrument = Instrument::new();
    let stream = stream_backfill(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &files,
        &opts,
        &instrument,
    )
    .unwrap();
    let mut prepared =
        prepare_captured_runtime(adapter, &runtime, &files, &mut planner, &stream).unwrap();
    prepared.mark_metadata_preapplied();
    apply_prepared_runtime(adapter, &runtime, prepared, canonical, &instrument).unwrap();
}

/// One runtime captured and prepared but not applied.
pub struct StreamedSetupAndSources {
    pub setup: Option<rosetta_pipeline::StreamedSetup>,
    pub emitted: Vec<Vec<ClosedSource>>,
    pub prepared: PreparedAdmission,
}

/// Capture and prepare one runtime's closed sources without applying them.
pub fn capture(
    planned: Planned,
    preparer: fn(&[SessionFile]) -> StreamedPreparer,
) -> (
    StreamedSetupAndSources,
    &'static dyn AgentAdapter,
    RuntimeRow,
) {
    let Planned {
        adapter,
        runtime,
        root,
        mut planner,
        files,
        opts,
    } = planned;
    let instrument = Instrument::new();
    let setup = streamed_setup(&files, &mut planner).unwrap();
    let mut preparer = preparer(&files);
    let mut emitted: Vec<Vec<ClosedSource>> = Vec::new();
    stream_closed_sources_into(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &files,
        &opts,
        &instrument,
        None,
        None,
        &ColdWalkOptions::default(),
        &mut BlockingEmit(|_, mut sources| {
            preparer.prepare(&mut sources)?;
            emitted.push(sources);
            Ok(())
        }),
    )
    .unwrap();
    (
        StreamedSetupAndSources {
            setup: Some(setup),
            emitted,
            prepared: preparer.finish().unwrap(),
        },
        adapter,
        runtime,
    )
}

/// The streamed path for one runtime as fresh serve ran it before R1:
/// admitted whole on the path walk, each emission applied as it arrives
/// after the producer-side preparer ranked it. Returns the runtime id, the
/// poll evidence its writer hands the runner, and the preparer's report.
pub fn stream_runtime(
    planned: Planned,
    canonical: &mut Store,
    preparer: fn(&[SessionFile]) -> StreamedPreparer,
) -> (String, HashMap<String, PollEvidence>, EarlyToolReport) {
    let Planned {
        adapter,
        runtime,
        root,
        mut planner,
        files,
        opts,
    } = planned;
    let instrument = Instrument::new();
    let setup = streamed_setup(&files, &mut planner).unwrap();
    let mut writer =
        StreamedRuntime::open(adapter, &runtime, setup, canonical, &instrument).unwrap();
    let mut preparer = preparer(&files);
    stream_closed_sources_into(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &files,
        &opts,
        &instrument,
        None,
        None,
        &ColdWalkOptions::default(),
        &mut BlockingEmit(|_, mut sources| {
            preparer.prepare(&mut sources)?;
            writer.apply(canonical, sources, &instrument).map(|_| ())
        }),
    )
    .unwrap();
    let evidence = writer.take_poll_evidence();
    let prepared = preparer.finish().unwrap();
    writer.admit(canonical, &instrument, &prepared).unwrap();
    (runtime.id, evidence, prepared.report())
}
