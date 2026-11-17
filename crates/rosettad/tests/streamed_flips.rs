//! T6 test 8: per-tree flips against one runtime admission.
//!
//! A root-tree runtime walked newest trees first, its sources applied in
//! 64 KiB slices, flipping its trees one at a time as they become ready —
//! leaving the last ready tree unflipped for the close — and then its
//! remainder, ends with exactly the canonical projection of the same
//! runtime admitted whole on the path walk: every table, every column, every
//! ordinal (surrogate ids mapped to stable identities). Generated corpora
//! only; never a home directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::{AgentAdapter, ColdAdmission};
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_core::rows::RuntimeRow;
use rosetta_pipeline::{
    BlockingEmit, ColdWalkOptions, FlipCaps, IngestOptions, Instrument, SessionFile, Step,
    StreamedPreparer, StreamedRuntime, discover, expand_data_root, meta_ingest_inventory_into,
    stream_closed_sources_into, streamed_setup, streamed_setup_for, walk_summary,
};
use rosetta_store::Store;
use rusqlite::types::Value;
use tempfile::TempDir;

struct Planned {
    adapter: &'static dyn AgentAdapter,
    runtime: RuntimeRow,
    root: PathBuf,
    planner: Store,
    files: Vec<SessionFile>,
    opts: IngestOptions,
}

fn copy_tree(from: &Path, to: &Path) {
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
fn fixture_roots(kind: &str) -> Vec<(String, PathBuf)> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth")
        .join(kind)
        .join("fixtures");
    let mut roots = Vec::new();
    for version in std::fs::read_dir(&fixtures).unwrap() {
        let version = version.unwrap().path();
        for scenario in std::fs::read_dir(&version).unwrap() {
            let scenario = scenario.unwrap().path();
            let Some(data) = std::fs::read_dir(&scenario)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.is_dir())
            else {
                continue;
            };
            roots.push((
                scenario.file_name().unwrap().to_string_lossy().into_owned(),
                data,
            ));
        }
    }
    roots.sort();
    roots
}

fn cold_store() -> Store {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    store
}

/// Discover, skim, and publish the metadata — fresh serve's steps before
/// capture.
fn plan(adapter: &'static dyn AgentAdapter, home: &Path, canonical: &mut Store) -> Planned {
    let opts = IngestOptions::default();
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
        None,
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

/// The reference: the path walk, admitted whole, every source unchunked.
fn admitted_whole(planned: Planned, canonical: &mut Store) {
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
    let mut preparer = StreamedPreparer::new(&files);
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
    let prepared = preparer.finish().unwrap();
    writer.admit(canonical, &instrument, &prepared).unwrap();
}

/// The per-tree build: newest trees first, every source an item stepped in
/// `chunk` slices, one tree flipped after each emission while more than one
/// is ready, then the remainder. Returns the trees flipped before the close.
fn flipped_per_tree(planned: Planned, canonical: &mut Store, chunk: u64) -> (usize, usize) {
    let Planned {
        adapter,
        runtime,
        root,
        mut planner,
        files,
        opts,
    } = planned;
    let instrument = Instrument::new();
    let setup = streamed_setup_for(&files, &mut planner, ColdAdmission::RootTree, false).unwrap();
    let trees = setup.trees().cloned().expect("a root-tree runtime");
    let walk = ColdWalkOptions::default();
    let summary = walk_summary(adapter, &files, Some(&trees), &walk);
    assert!(!summary.is_path(), "a recency walk");
    let mut writer =
        StreamedRuntime::open(adapter, &runtime, setup, canonical, &instrument).unwrap();
    let mut preparer = StreamedPreparer::with_trees(&files, trees.clone(), summary.lane_of_tree);
    let one_tree = FlipCaps {
        trees: 1,
        sessions: usize::MAX,
        tail_bytes: u64::MAX,
    };
    let mut flipped = 0;
    let mut sliced = 0;
    stream_closed_sources_into(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &files,
        &opts,
        &instrument,
        None,
        Some(&trees),
        &walk,
        &mut BlockingEmit(|_, mut sources| {
            preparer.prepare(&mut sources)?;
            for source in sources {
                let bytes = source.budget_bytes();
                let mut item = writer.begin_item(vec![source], bytes, chunk)?;
                let mut released = 0;
                loop {
                    let (step, release, _) = writer.step(canonical, &mut item, &instrument)?;
                    released += release;
                    if step == Step::Done {
                        break;
                    }
                    sliced += 1;
                }
                assert_eq!(released, bytes);
            }
            // Keep the last ready tree for the close.
            while writer.ready_trees() > 1 {
                let admitted = writer.admit_trees(canonical, &instrument, one_tree)?;
                assert_eq!(admitted.trees.len(), 1);
                flipped += 1;
            }
            Ok(())
        }),
    )
    .unwrap();
    let prepared = preparer.finish().unwrap();
    let changed = writer
        .admit_remainder(canonical, &instrument, &prepared)
        .unwrap()
        .changed;
    assert!(!changed.is_empty(), "the remainder flips what is left");
    (flipped, sliced)
}

/// Every canonical table with surrogate ids mapped to stable identities.
fn dump(store: &Store) -> BTreeMap<String, Vec<String>> {
    let conn = store.conn();
    let cursors: BTreeMap<i64, String> = conn
        .prepare("SELECT id, runtime_id, path FROM ingest_cursor")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                format!("{}|{}", row.get::<_, String>(1)?, row.get::<_, String>(2)?),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let messages: BTreeMap<i64, String> = conn
        .prepare("SELECT id, timeline_id FROM message")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != 'source_audit'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut out = BTreeMap::new();
    for table in tables {
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter(|column| {
                !((table == "message" || table == "ingest_cursor") && column == "id")
                    && !(table == "projection_state"
                        && (column == "projection_epoch" || column == "artifacts_revision"))
                    && !(table == "agent_runtime" && column == "discovered_at")
            })
            .collect();
        let select = columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(",");
        let mut rows: Vec<String> = conn
            .prepare(&format!("SELECT {select} FROM \"{table}\""))
            .unwrap()
            .query_map([], |row| {
                let mut fields = Vec::with_capacity(columns.len());
                for (index, column) in columns.iter().enumerate() {
                    let value: Value = row.get(index)?;
                    let text = match (&value, column.as_str()) {
                        (Value::Integer(id), name) if name.ends_with("cursor") => cursors
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| format!("<dangling cursor {id}>")),
                        (Value::Integer(id), "message_id") => messages
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| format!("<dangling message {id}>")),
                        _ => format!("{value:?}"),
                    };
                    fields.push(format!("{column}={text}"));
                }
                Ok(fields.join(" | "))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows.sort();
        out.insert(table, rows);
    }
    out
}

fn assert_same(label: &str, reference: &Store, candidate: &Store) {
    let (reference, candidate) = (dump(reference), dump(candidate));
    assert_eq!(
        reference.keys().collect::<Vec<_>>(),
        candidate.keys().collect::<Vec<_>>()
    );
    for (table, expected) in &reference {
        let actual = &candidate[table];
        if expected != actual {
            let missing: Vec<_> = expected
                .iter()
                .filter(|row| !actual.contains(row))
                .take(3)
                .collect();
            let extra: Vec<_> = actual
                .iter()
                .filter(|row| !expected.contains(row))
                .take(3)
                .collect();
            panic!(
                "{label}: table {table} differs\nadmitted whole only: {missing:#?}\n\
                 flipped only: {extra:#?}"
            );
        }
    }
}

#[test]
fn flipping_trees_one_by_one_equals_one_runtime_admission() {
    for (adapter, agent, seed) in [
        (
            &ClaudeCodeAdapter as &'static dyn AgentAdapter,
            Agent::ClaudeCode,
            73,
        ),
        (&CodexAdapter, Agent::Codex, 19),
    ] {
        let home = TempDir::new().unwrap();
        let root = expand_data_root(&adapter.layout().data_root, home.path());
        std::fs::create_dir_all(&root).unwrap();
        generate(&CorpusSpec::new(agent, 12, 1_500, seed), &root).unwrap();

        let mut reference = cold_store();
        let planned = plan(adapter, home.path(), &mut reference);
        admitted_whole(planned, &mut reference);
        reference.finish_bulk_load().unwrap();

        let mut candidate = cold_store();
        let planned = plan(adapter, home.path(), &mut candidate);
        let (flipped, sliced) = flipped_per_tree(planned, &mut candidate, 64 * 1024);
        assert!(sliced > 0, "some sources were applied in slices");
        candidate.finish_bulk_load().unwrap();
        assert!(
            flipped > 0,
            "{}: trees flipped before the close",
            adapter.kind()
        );
        assert_same(adapter.kind(), &reference, &candidate);
    }
}

/// The same equality over every recorded Claude Code and Codex fixture:
/// subagents, sidecars, guardians and state members close their trees
/// without a closure violation, and the per-tree build ends where the
/// runtime admission does.
#[test]
fn per_tree_flips_equal_runtime_admission_for_every_root_tree_fixture() {
    let mut compared = 0;
    for adapter in [
        &ClaudeCodeAdapter as &'static dyn AgentAdapter,
        &CodexAdapter,
    ] {
        for (scenario, data) in fixture_roots(adapter.kind()) {
            let label = format!("{}/{scenario}", adapter.kind());
            let home = TempDir::new().unwrap();
            copy_tree(
                &data,
                &expand_data_root(&adapter.layout().data_root, home.path()),
            );

            let mut reference = cold_store();
            let planned = plan(adapter, home.path(), &mut reference);
            if planned.files.is_empty() {
                continue;
            }
            admitted_whole(planned, &mut reference);
            reference.finish_bulk_load().unwrap();

            let mut candidate = cold_store();
            let planned = plan(adapter, home.path(), &mut candidate);
            flipped_per_tree(planned, &mut candidate, 4 * 1024);
            candidate.finish_bulk_load().unwrap();
            assert_same(&label, &reference, &candidate);
            compared += 1;
        }
    }
    assert!(compared >= 6, "only {compared} fixture scenarios compared");
}
