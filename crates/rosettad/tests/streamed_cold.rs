//! Streamed fresh-cold apply against the retained reference.
//!
//! The retained path (`prepare_captured_runtime` + `apply_prepared_runtime`,
//! what fresh `serve` ran before streaming) is the reference: a streamed
//! runtime must produce the same canonical projection — every table, every
//! column, every message ordinal — while never exposing a runtime before its
//! admission and never folding another runtime's facts.

use std::collections::{BTreeMap, HashMap};
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
    ClosedSource, EarlyToolReport, IngestOptions, Instrument, PollEvidence, PreparedAdmission,
    SessionFile, StreamedPreparer, StreamedRuntime, apply_prepared_runtime, discover,
    expand_data_root, meta_ingest_inventory_into, prepare_captured_runtime, stream_backfill,
    stream_closed_sources_into, streamed_setup,
};
use rosetta_store::Store;
use rusqlite::types::Value;
use tempfile::TempDir;

fn adapters() -> Vec<&'static dyn AgentAdapter> {
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
fn home_with(adapter: &dyn AgentAdapter, data: &Path) -> TempDir {
    let home = TempDir::new().unwrap();
    copy_tree(
        data,
        &expand_data_root(&adapter.layout().data_root, home.path()),
    );
    home
}

struct Planned {
    adapter: &'static dyn AgentAdapter,
    runtime: RuntimeRow,
    root: PathBuf,
    planner: Store,
    files: Vec<SessionFile>,
    opts: IngestOptions,
}

/// Discover, open a cold planner, skim metadata, and publish the metadata to
/// the canonical store — fresh serve's steps before capture.
fn plan(adapter: &'static dyn AgentAdapter, home: &Path, canonical: &mut Store) -> Planned {
    plan_with(adapter, home, canonical, IngestOptions::default())
}

fn plan_with(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    canonical: &mut Store,
    opts: IngestOptions,
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

fn cold_store() -> Store {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    store
}

/// The retained reference: capture everything, prepare, apply, fold.
fn retained(planned: Planned, canonical: &mut Store) {
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

/// Capture and prepare one runtime's closed sources without applying them.
fn capture(
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
        &mut |mut sources| {
            preparer.prepare(&mut sources)?;
            emitted.push(sources);
            Ok(())
        },
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

struct StreamedSetupAndSources {
    setup: Option<rosetta_pipeline::StreamedSetup>,
    emitted: Vec<Vec<ClosedSource>>,
    prepared: PreparedAdmission,
}

/// The streamed path for one runtime as fresh serve runs it, applying each
/// emission as it arrives after the producer-side preparer has ranked it and
/// resolved every session it can prove self-contained. Returns the runtime id
/// and the poll evidence its writer hands the runner.
fn streamed(planned: Planned, canonical: &mut Store) -> (String, HashMap<String, PollEvidence>) {
    let (runtime_id, evidence, _) = stream_runtime(planned, canonical, StreamedPreparer::new);
    (runtime_id, evidence)
}

/// The streamed path with every tool lifecycle left to the admission fold.
fn streamed_fold(planned: Planned, canonical: &mut Store) {
    stream_runtime(planned, canonical, StreamedPreparer::without_early_tools);
}

/// The streamed path with early tool resolution, reporting what it resolved.
fn streamed_early(planned: Planned, canonical: &mut Store) -> (String, EarlyToolReport) {
    let (runtime_id, _, report) = stream_runtime(planned, canonical, StreamedPreparer::new);
    (runtime_id, report)
}

fn stream_runtime(
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
        &mut |mut sources| {
            preparer.prepare(&mut sources)?;
            writer.apply(canonical, sources, &instrument).map(|_| ())
        },
    )
    .unwrap();
    let evidence = writer.take_poll_evidence();
    let prepared = preparer.finish().unwrap();
    writer.admit(canonical, &instrument, &prepared).unwrap();
    (runtime.id, evidence, prepared.report())
}

/// The live runner trusts its seed as if it had read each cursor row: after
/// admission and finalization, every cursor of the runtime must read back as
/// exactly the evidence the writer handed over.
fn assert_seed_matches_cursors(
    label: &str,
    store: &Store,
    runtime_id: &str,
    seed: &HashMap<String, PollEvidence>,
) {
    let paths: Vec<String> = store
        .conn()
        .prepare("SELECT path FROM ingest_cursor WHERE runtime_id = ?1")
        .unwrap()
        .query_map([runtime_id], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        seed.len(),
        paths.len(),
        "{label}: the seed covers every cursor"
    );
    for path in &paths {
        assert_eq!(
            seed.get(path).copied(),
            Some(PollEvidence::read(store, runtime_id, path).unwrap()),
            "{label}: {path}"
        );
    }
}

/// Every canonical table with surrogate ids mapped to stable identities —
/// the same normalization as the frozen-snapshot digest.
fn dump_ignoring(store: &Store, ignored: &[(&str, &str)]) -> BTreeMap<String, Vec<String>> {
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
                    && !(table == "projection_state" && column == "projection_epoch")
                    && !(table == "agent_runtime" && column == "discovered_at")
                    && !ignored.contains(&(table.as_str(), column.as_str()))
            })
            .collect();
        let select = columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(",");
        let mut statement = conn
            .prepare(&format!("SELECT {select} FROM \"{table}\""))
            .unwrap();
        let mut rows = statement
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
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        out.insert(table, rows);
    }
    out
}

fn assert_same_projection(label: &str, reference: &Store, candidate: &Store) {
    assert_same_projection_ignoring(label, reference, candidate, &[]);
}

fn dump(store: &Store) -> BTreeMap<String, Vec<String>> {
    dump_ignoring(store, &[])
}

fn assert_same_projection_ignoring(
    label: &str,
    reference: &Store,
    candidate: &Store,
    ignored: &[(&str, &str)],
) {
    let (reference, candidate) = if ignored.is_empty() {
        (dump(reference), dump(candidate))
    } else {
        (
            dump_ignoring(reference, ignored),
            dump_ignoring(candidate, ignored),
        )
    };
    assert_eq!(
        reference.keys().collect::<Vec<_>>(),
        candidate.keys().collect::<Vec<_>>(),
        "{label}: table sets differ"
    );
    for (table, expected) in &reference {
        let actual = &candidate[table];
        if expected != actual {
            let missing = expected
                .iter()
                .filter(|row| !actual.contains(row))
                .take(3)
                .collect::<Vec<_>>();
            let extra = actual
                .iter()
                .filter(|row| !expected.contains(row))
                .take(3)
                .collect::<Vec<_>>();
            panic!(
                "{label}: table {table} differs ({} reference rows, {} streamed rows)\n\
                 only in reference: {missing:#?}\nonly in streamed: {extra:#?}",
                expected.len(),
                actual.len()
            );
        }
    }
}

fn message_count(store: &Store) -> i64 {
    store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn streamed_projection_matches_the_retained_projection_for_every_fixture() {
    let mut compared = 0;
    for adapter in adapters() {
        for (scenario, data) in fixture_roots(adapter.kind()) {
            let label = format!("{}/{scenario}", adapter.kind());
            let home = home_with(adapter, &data);

            let mut reference = cold_store();
            let planned = plan(adapter, home.path(), &mut reference);
            retained(planned, &mut reference);
            reference.finish_bulk_load().unwrap();

            let mut candidate = cold_store();
            let planned = plan(adapter, home.path(), &mut candidate);
            let (runtime_id, seed) = streamed(planned, &mut candidate);
            candidate.finish_bulk_load().unwrap();

            assert_same_projection(&label, &reference, &candidate);
            assert_seed_matches_cursors(&label, &candidate, &runtime_id, &seed);
            compared += 1;
        }
    }
    assert!(compared >= 27, "only {compared} fixture scenarios compared");
}

#[test]
fn streamed_projection_matches_for_generated_multi_session_corpora() {
    use rosetta_bench_corpus::{Agent, CorpusSpec, generate};

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
        generate(&CorpusSpec::new(agent, 12, 4_000, seed), &root).unwrap();

        let mut reference = cold_store();
        let planned = plan(adapter, home.path(), &mut reference);
        retained(planned, &mut reference);
        reference.finish_bulk_load().unwrap();

        let mut candidate = cold_store();
        let planned = plan(adapter, home.path(), &mut candidate);
        let (runtime_id, seed) = streamed(planned, &mut candidate);
        candidate.finish_bulk_load().unwrap();
        assert_seed_matches_cursors(adapter.kind(), &candidate, &runtime_id, &seed);

        assert!(
            message_count(&reference) > 500,
            "{} messages",
            message_count(&reference)
        );
        assert_same_projection(adapter.kind(), &reference, &candidate);
    }
}

#[test]
fn tool_rows_resolved_at_file_close_equal_the_admission_fold_for_every_fixture() {
    let mut resolved = EarlyToolReport::default();
    let mut compared = 0;
    for adapter in adapters() {
        for (scenario, data) in fixture_roots(adapter.kind()) {
            let label = format!("{}/{scenario}", adapter.kind());
            let home = home_with(adapter, &data);

            let mut folded = cold_store();
            let planned = plan(adapter, home.path(), &mut folded);
            streamed_fold(planned, &mut folded);
            folded.finish_bulk_load().unwrap();

            let mut early = cold_store();
            let planned = plan(adapter, home.path(), &mut early);
            let (_, report) = streamed_early(planned, &mut early);
            early.finish_bulk_load().unwrap();

            assert_same_projection(&label, &folded, &early);
            resolved.sessions += report.sessions;
            resolved.tool_rows += report.tool_rows;
            resolved.withheld_sources += report.withheld_sources;
            compared += 1;
        }
    }
    assert!(compared >= 27, "only {compared} fixture scenarios compared");
    // The comparison is not vacuous: fixtures resolve sessions early.
    assert!(
        resolved.sessions > 0 && resolved.tool_rows > 0,
        "{resolved:?}"
    );
}

#[test]
fn tool_rows_resolved_at_file_close_equal_the_admission_fold_for_generated_corpora() {
    use rosetta_bench_corpus::{Agent, CorpusSpec, generate};

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
        generate(&CorpusSpec::new(agent, 12, 4_000, seed), &root).unwrap();

        let mut reference = cold_store();
        let planned = plan(adapter, home.path(), &mut reference);
        retained(planned, &mut reference);
        reference.finish_bulk_load().unwrap();

        let mut folded = cold_store();
        let planned = plan(adapter, home.path(), &mut folded);
        streamed_fold(planned, &mut folded);
        folded.finish_bulk_load().unwrap();

        let mut early = cold_store();
        let planned = plan(adapter, home.path(), &mut early);
        let (runtime_id, report) = streamed_early(planned, &mut early);
        early.finish_bulk_load().unwrap();

        assert_same_projection(adapter.kind(), &folded, &early);
        assert_same_projection(adapter.kind(), &reference, &early);
        eprintln!(
            "{}: {} of {} tool rows resolved at file close ({report:?})",
            adapter.kind(),
            report.tool_rows,
            runtime_tool_rows(&early, &runtime_id)
        );
    }
}

/// One Codex guardian review rollout (adapter audit E1): a ledger-only thread
/// whose token counts become `SessionUsage` samples on `parent`.
fn write_guardian_review(dir: &Path, stamp: &str, review: &str, parent: &str, cwd: &str) {
    let usage = |input: i64, output: i64| {
        serde_json::json!({
            "input_tokens": input, "cached_input_tokens": 0, "cache_write_input_tokens": 0,
            "output_tokens": output, "reasoning_output_tokens": 0, "total_tokens": input + output,
        })
    };
    let records = [
        serde_json::json!({
            "timestamp": "2026-08-25T02:36:00.000Z", "ordinal": 0, "type": "session_meta",
            "payload": {
                "id": review, "timestamp": "2026-08-25T02:36:00.000Z", "cwd": cwd,
                "originator": "codex_exec", "cli_version": "0.149.1",
                "source": {"subagent": {"other": "guardian"}},
                "thread_source": "guardian_review", "model_provider": "openai",
                "parent_thread_id": parent,
            },
        }),
        serde_json::json!({
            "timestamp": "2026-08-25T02:36:01.000Z", "ordinal": 1, "type": "turn_context",
            "payload": {"cwd": cwd, "model": "codex-auto-review"},
        }),
        serde_json::json!({
            "timestamp": "2026-08-25T02:36:05.000Z", "ordinal": 2, "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "total_token_usage": usage(300, 30), "last_token_usage": usage(300, 30),
            }},
        }),
    ];
    let text = records
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(dir.join(format!("rollout-{stamp}-{review}.jsonl")), text).unwrap();
}

/// Adapter audit E1 end to end, with early tools on as fresh serve runs them:
/// Codex guardian reviews — ledger-only rollouts that name their parent thread
/// only through usage samples, one sorting before and one after the parent's
/// rollout — leave the parent self-contained. Its tool rows resolve on the
/// producer, the samples land on it, and the projection equals the fold path's.
#[test]
fn codex_guardian_reviews_leave_their_parent_resolved_like_the_fold() {
    let (_, data) = fixture_roots("codex")
        .into_iter()
        .find(|(scenario, _)| scenario == "tool-use")
        .expect("the Codex tool-use fixture");
    let home = home_with(&CodexAdapter, &data);
    let root = expand_data_root(&CodexAdapter.layout().data_root, home.path());
    let rollout = walkdir(&root)
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("rollout-"))
        })
        .expect("the fixture's parent rollout");
    let header: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(&rollout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let parent = header["payload"]["id"].as_str().unwrap().to_owned();
    let cwd = header["payload"]["cwd"].as_str().unwrap().to_owned();
    let dir = rollout.parent().unwrap();
    write_guardian_review(
        dir,
        "2026-08-24T19-30-00",
        "0199aaaa-0000-7000-8000-00000000000a",
        &parent,
        &cwd,
    );
    write_guardian_review(
        dir,
        "2026-08-24T19-40-00",
        "0199aaaa-0000-7000-8000-00000000000b",
        &parent,
        &cwd,
    );

    let mut folded = cold_store();
    let planned = plan(&CodexAdapter, home.path(), &mut folded);
    assert_eq!(
        planned.files.iter().filter(|file| file.ledger_only).count(),
        2,
        "both reviews are ledger-only roster sources"
    );
    streamed_fold(planned, &mut folded);
    folded.finish_bulk_load().unwrap();

    let mut early = cold_store();
    let planned = plan(&CodexAdapter, home.path(), &mut early);
    let (_, report) = streamed_early(planned, &mut early);
    early.finish_bulk_load().unwrap();

    // The samples never cross the parent: it resolves before admission.
    assert_eq!(report.crossed_sessions, 0, "{report:?}");
    assert_eq!(report.sessions, 1, "{report:?}");
    assert!(report.tool_rows > 0, "{report:?}");
    // Both reviews' samples landed on the parent session.
    let samples: i64 = early
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM session_usage u
             JOIN ingest_cursor review ON review.id = u.src_cursor
             JOIN ingest_cursor rollout ON rollout.session_id = u.session_id
             WHERE review.session_id IS NULL
               AND rollout.path LIKE '%' || ?1 || '.jsonl'",
            [&parent],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(samples, 2);
    assert_same_projection("codex guardian reviews", &folded, &early);
}

#[test]
fn a_restarted_two_transcript_session_prepares_like_the_fold() {
    let (home, opts) = restarting_home();
    let mut folded = cold_store();
    let planned = plan_with(&ClaudeCodeAdapter, home.path(), &mut folded, opts);
    streamed_fold(planned, &mut folded);
    folded.finish_bulk_load().unwrap();

    let (home, opts) = restarting_home();
    let mut early = cold_store();
    let planned = plan_with(&ClaudeCodeAdapter, home.path(), &mut early, opts);
    let (_, report) = streamed_early(planned, &mut early);
    early.finish_bulk_load().unwrap();

    // Both healed transcripts were withheld until the session completed.
    assert_eq!(report.withheld_sources, 2, "{report:?}");
    assert_same_projection_ignoring(
        "two transcripts",
        &folded,
        &early,
        &[("ingest_cursor", "checkpoint"), ("session", "updated_at")],
    );
}

fn runtime_tool_rows(store: &Store, runtime_id: &str) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM message m JOIN session s ON s.id = m.session_id
             WHERE s.runtime_id = ?1 AND m.type = 'tool_use'",
            [runtime_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn runtime_pending_sessions(store: &Store, runtime_id: &str) -> (i64, i64) {
    store
        .conn()
        .query_row(
            "SELECT COUNT(*), SUM(stream_state = 'pending') FROM session WHERE runtime_id = ?1",
            [runtime_id],
            |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )
        .unwrap()
}

#[test]
fn admitting_one_runtime_never_folds_another_runtimes_facts() {
    use rosetta_bench_corpus::{Agent, CorpusSpec, generate};

    let home = TempDir::new().unwrap();
    for (adapter, agent, seed) in [
        (
            &ClaudeCodeAdapter as &'static dyn AgentAdapter,
            Agent::ClaudeCode,
            5,
        ),
        (&CodexAdapter, Agent::Codex, 6),
    ] {
        let root = expand_data_root(&adapter.layout().data_root, home.path());
        std::fs::create_dir_all(&root).unwrap();
        generate(&CorpusSpec::new(agent, 4, 180, seed), &root).unwrap();
    }

    // Reference: both runtimes retained and applied one after the other.
    let mut reference = cold_store();
    let claude = plan(&ClaudeCodeAdapter, home.path(), &mut reference);
    let codex = plan(&CodexAdapter, home.path(), &mut reference);
    retained(codex, &mut reference);
    retained(claude, &mut reference);
    reference.finish_bulk_load().unwrap();

    // Streamed: both runtimes' sources interleave on one writer; Codex is
    // admitted while every Claude source (with its unresolved tool facts)
    // has already landed.
    let mut candidate = cold_store();
    let claude = plan(&ClaudeCodeAdapter, home.path(), &mut candidate);
    let codex = plan(&CodexAdapter, home.path(), &mut candidate);
    let instrument = Instrument::new();
    // Every tool lifecycle is left to the admission folds here: the
    // property under test is that one runtime's fold never takes another's.
    let (mut claude_capture, claude_adapter, claude_runtime) =
        capture(claude, StreamedPreparer::without_early_tools);
    let (mut codex_capture, codex_adapter, codex_runtime) =
        capture(codex, StreamedPreparer::without_early_tools);
    let mut claude_writer = StreamedRuntime::open(
        claude_adapter,
        &claude_runtime,
        claude_capture.setup.take().unwrap(),
        &mut candidate,
        &instrument,
    )
    .unwrap();
    let mut codex_writer = StreamedRuntime::open(
        codex_adapter,
        &codex_runtime,
        codex_capture.setup.take().unwrap(),
        &mut candidate,
        &instrument,
    )
    .unwrap();
    let mut claude_sources = claude_capture.emitted.into_iter();
    let mut codex_sources = codex_capture.emitted.into_iter();
    loop {
        let claude_next = claude_sources.next();
        let codex_next = codex_sources.next();
        if claude_next.is_none() && codex_next.is_none() {
            break;
        }
        if let Some(sources) = claude_next {
            claude_writer
                .apply(&mut candidate, sources, &instrument)
                .unwrap();
        }
        if let Some(sources) = codex_next {
            codex_writer
                .apply(&mut candidate, sources, &instrument)
                .unwrap();
        }
    }
    assert!(claude_writer.sources_applied() > 1);
    assert_eq!(runtime_tool_rows(&candidate, &claude_runtime.id), 0);
    assert_eq!(runtime_tool_rows(&candidate, &codex_runtime.id), 0);

    codex_writer
        .admit(&mut candidate, &instrument, &codex_capture.prepared)
        .unwrap();
    assert!(runtime_tool_rows(&candidate, &codex_runtime.id) > 0);
    // Claude's deferred facts stayed in Claude's buffer: no Claude tool row
    // was folded or written by Codex's admission, and Claude is still behind
    // the Pending barrier.
    assert_eq!(runtime_tool_rows(&candidate, &claude_runtime.id), 0);
    let (sessions, pending) = runtime_pending_sessions(&candidate, &claude_runtime.id);
    assert!(sessions > 0);
    assert_eq!(pending, sessions);
    let (_, codex_pending) = runtime_pending_sessions(&candidate, &codex_runtime.id);
    assert_eq!(codex_pending, 0);

    claude_writer
        .admit(&mut candidate, &instrument, &claude_capture.prepared)
        .unwrap();
    assert!(runtime_tool_rows(&candidate, &claude_runtime.id) > 0);
    candidate.finish_bulk_load().unwrap();
    assert_same_projection("claude+codex interleaved", &reference, &candidate);
}

/// Rewrites one source the first time capture reports bytes for it — after
/// its first batch was framed and before the next read — so the tailer sees
/// a changed anchor and restarts the generation mid-pass.
#[derive(Debug)]
struct RewriteOnce {
    root: PathBuf,
    target: String,
    replacement: String,
    done: std::sync::atomic::AtomicBool,
}

impl rosetta_pipeline::progress::ColdProgressObserver for RewriteOnce {
    fn source_bytes(&self, file: &SessionFile, _bytes: u64) {
        if file.relative_path == self.target
            && !self.done.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            std::fs::write(self.root.join(&self.target), &self.replacement).unwrap();
        }
    }

    fn reading_finished(&self) {}
}

fn claude_user(uuid: &str, content: &str) -> String {
    serde_json::json!({
        "parentUuid": null,
        "isSidechain": false,
        "type": "user",
        "message": {"role": "user", "content": content},
        "uuid": uuid,
        "timestamp": "2026-08-25T02:35:19.054Z",
        "userType": "external",
        "cwd": "/w/heal",
        "sessionId": "heal-session",
        "version": "2.1.241",
        "gitBranch": "main"
    })
    .to_string()
}

const HEAL_TRANSCRIPT: &str = "projects/-w-heal/heal-session.jsonl";
const HEAL_JOURNAL: &str =
    "projects/-w-heal/heal-session/subagents/workflows/wf_heal/journal.jsonl";

/// A two-transcript session (transcript + workflow journal) whose transcript
/// is rewritten between its first and second capture batch.
fn restarting_home() -> (TempDir, IngestOptions) {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path());
    let lines = |prefix: &str| {
        (1..=5)
            .map(|index| {
                claude_user(
                    &format!("{prefix}-{index}"),
                    &format!("{prefix} line {index}"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    let transcript = root.join(HEAL_TRANSCRIPT);
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, lines("old")).unwrap();
    let journal = root.join(HEAL_JOURNAL);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        [
            r#"{"type":"started","key":"worker-1","agentId":"agent-1"}"#,
            r#"{"type":"result","key":"worker-1","result":{"ok":true}}"#,
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let mut tail = rosetta_capture::TailConfig::json_lines();
    tail.max_records_per_batch = 2;
    let opts = IngestOptions {
        tail,
        progress: Some(std::sync::Arc::new(RewriteOnce {
            root,
            target: HEAL_TRANSCRIPT.into(),
            replacement: lines("new"),
            done: std::sync::atomic::AtomicBool::new(false),
        })),
        ..IngestOptions::default()
    };
    (home, opts)
}

#[test]
fn a_restarted_session_heals_before_its_sources_stream() {
    let (reference_home, reference_opts) = restarting_home();
    let mut reference = cold_store();
    let planned = plan_with(
        &ClaudeCodeAdapter,
        reference_home.path(),
        &mut reference,
        reference_opts,
    );
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
    assert_eq!(
        stream.generations_restarted, 2,
        "the restart and the forced heal"
    );
    assert_eq!(stream.siblings_healed, 1);
    let mut prepared =
        prepare_captured_runtime(adapter, &runtime, &files, &mut planner, &stream).unwrap();
    prepared.mark_metadata_preapplied();
    apply_prepared_runtime(adapter, &runtime, prepared, &mut reference, &instrument).unwrap();
    reference.finish_bulk_load().unwrap();

    let (home, opts) = restarting_home();
    let mut candidate = cold_store();
    let Planned {
        adapter,
        runtime,
        root,
        mut planner,
        files,
        opts,
    } = plan_with(&ClaudeCodeAdapter, home.path(), &mut candidate, opts);
    let setup = streamed_setup(&files, &mut planner).unwrap();
    let mut writer =
        StreamedRuntime::open(adapter, &runtime, setup, &mut candidate, &instrument).unwrap();
    let mut preparer = StreamedPreparer::new(&files);
    let mut emissions = Vec::new();
    let report = stream_closed_sources_into(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &files,
        &opts,
        &instrument,
        None,
        &mut |mut sources| {
            emissions.push(
                sources
                    .iter()
                    .map(|source| source.relative_path().to_owned())
                    .collect::<Vec<_>>(),
            );
            preparer.prepare(&mut sources)?;
            writer
                .apply(&mut candidate, sources, &instrument)
                .map(|_| ())
        },
    )
    .unwrap();
    assert_eq!(
        report.generations_restarted, 2,
        "the restart and the forced heal"
    );
    assert_eq!(report.siblings_healed, 1);
    // The session's two transcripts were held until the heal settled them,
    // then emitted once each, in path order.
    assert_eq!(
        emissions,
        vec![vec![HEAL_TRANSCRIPT.to_owned(), HEAL_JOURNAL.to_owned()]]
    );
    writer
        .admit(&mut candidate, &instrument, &preparer.finish().unwrap())
        .unwrap();
    candidate.finish_bulk_load().unwrap();

    let generations: Vec<(String, i64, i64)> = candidate
        .conn()
        .prepare(
            "SELECT path, active_generation, highest_generation FROM ingest_cursor ORDER BY path",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        generations,
        vec![
            (HEAL_TRANSCRIPT.to_owned(), 2, 2),
            (HEAL_JOURNAL.to_owned(), 2, 2),
        ]
    );
    let old_rows: i64 = candidate
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM message WHERE content LIKE 'old line%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_rows, 0);
    // Two homes (the rewrite mutates its source): tail checkpoints differ by
    // inode and skimmed session times by file mtime alone. Generations are
    // asserted above.
    assert_same_projection_ignoring(
        "restart + sibling heal",
        &reference,
        &candidate,
        &[("ingest_cursor", "checkpoint"), ("session", "updated_at")],
    );
}

fn get(router: &axum::Router, uri: &str) -> (u16, serde_json::Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::get(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, body)
    })
}

fn one_session_with_rows(store: &Store, runtime_id: &str) -> (String, String) {
    store
        .conn()
        .query_row(
            "SELECT m.session_id, m.timeline_id FROM message m
             JOIN session s ON s.id = m.session_id
             WHERE s.runtime_id = ?1 AND m.type IN ('user', 'agent')
             ORDER BY m.session_id, m.timeline_id LIMIT 1",
            [runtime_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn streamed_rows_stay_behind_the_pending_barrier_until_admission() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let (_, claude_data) = fixture_roots("claude-code")
        .into_iter()
        .find(|(scenario, _)| scenario == "tool-use")
        .unwrap();
    copy_tree(
        &claude_data,
        &expand_data_root(&ClaudeCodeAdapter.layout().data_root, &home),
    );
    let (_, grok_data) = fixture_roots("grok")
        .into_iter()
        .find(|(scenario, _)| scenario == "tool-use")
        .unwrap();
    let grok_root = expand_data_root(&GrokAdapter.layout().data_root, &home);
    copy_tree(&grok_data, &grok_root);
    // One plan update, so the plan-revision route has a streamed row to hide.
    let updates = walkdir(&grok_root)
        .into_iter()
        .find(|path| path.ends_with("updates.jsonl"))
        .unwrap();
    let native = updates
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let plan_line = serde_json::json!({
        "timestamp": 1787634573,
        "method": "session/update",
        "params": {
            "sessionId": native,
            "update": {
                "sessionUpdate": "plan",
                "entries": [{"content": "hidden step", "status": "pending", "priority": "medium"}],
            },
            "_meta": {"eventId": format!("{native}-plan"), "agentTimestampMs": 1787634573000_i64},
        },
    });
    let mut text = std::fs::read_to_string(&updates).unwrap();
    text.push_str(&format!("{plan_line}\n"));
    std::fs::write(&updates, text).unwrap();

    // Fresh serve's canonical RAM projection and read router.
    let uri = format!(
        "file:rosetta-streamed-barrier-{}-{}?mode=memory&cache=shared",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let db = temp.path().join("projection.sqlite");
    let mut store = Store::open_shared_in_memory_for_rebuild_with_assets(&uri, &db).unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    store.observe_meta_effects();
    let reader =
        std::sync::Arc::new(rosetta_server::ProjectionReader::shared_memory(&uri).unwrap());
    let hub = std::sync::Arc::new(rosetta_server::Hub::new(
        rosetta_server::HubConfig::default(),
        1,
    ));
    let router = rosetta_server::router(
        rosetta_server::AppState::new(hub, db.clone()).with_projection(reader),
    );

    let claude = plan(&ClaudeCodeAdapter, &home, &mut store);
    let grok = plan(&GrokAdapter, &home, &mut store);
    let claude_id = claude.runtime.id.clone();
    let grok_id = grok.runtime.id.clone();
    streamed(claude, &mut store);

    let instrument = Instrument::new();
    let (mut grok_capture, grok_adapter, grok_runtime) = capture(grok, StreamedPreparer::new);
    let mut grok_writer = StreamedRuntime::open(
        grok_adapter,
        &grok_runtime,
        grok_capture.setup.take().unwrap(),
        &mut store,
        &instrument,
    )
    .unwrap();
    for sources in std::mem::take(&mut grok_capture.emitted) {
        grok_writer.apply(&mut store, sources, &instrument).unwrap();
    }
    // Tool rows resolved before admission are in SQLite too, and just as
    // hidden as every other row below.
    assert!(runtime_tool_rows(&store, &grok_id) > 0);

    // Grok's rows are in SQLite, but its sessions are still Pending.
    let (session, timeline) = one_session_with_rows(&store, &grok_id);
    let plans: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM plan WHERE session_id = ?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(plans, 1, "the streamed plan row landed");
    let (status, page) = get(&router, &format!("/api/sessions/{session}/timeline"));
    assert_eq!(status, 200);
    assert_eq!(page["entries"], serde_json::json!([]));
    let (status, _) = get(
        &router,
        &format!("/api/sessions/{session}/timeline/{timeline}"),
    );
    assert_eq!(status, 404);
    let (status, collaboration) = get(&router, &format!("/api/sessions/{session}/collaboration"));
    assert_eq!(status, 200);
    assert_eq!(collaboration["entries"], serde_json::json!([]));
    let (status, _) = get(
        &router,
        &format!("/api/sessions/{session}/plans/revisions/0"),
    );
    assert_eq!(status, 404);
    // The admitted runtime reads normally beside it.
    let (claude_session, claude_timeline) = one_session_with_rows(&store, &claude_id);
    let (status, page) = get(&router, &format!("/api/sessions/{claude_session}/timeline"));
    assert_eq!(status, 200);
    assert!(!page["entries"].as_array().unwrap().is_empty());
    let (status, _) = get(
        &router,
        &format!("/api/sessions/{claude_session}/timeline/{claude_timeline}"),
    );
    assert_eq!(status, 200);

    grok_writer
        .admit(&mut store, &instrument, &grok_capture.prepared)
        .unwrap();
    let (status, page) = get(&router, &format!("/api/sessions/{session}/timeline"));
    assert_eq!(status, 200);
    assert!(!page["entries"].as_array().unwrap().is_empty());
    let (status, _) = get(
        &router,
        &format!("/api/sessions/{session}/timeline/{timeline}"),
    );
    assert_eq!(status, 200);
    let (status, _) = get(
        &router,
        &format!("/api/sessions/{session}/plans/revisions/0"),
    );
    assert_eq!(status, 200);
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
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
