//! The pooled walk against the serial walk it replaces. On every recorded
//! fixture of every adapter, generated multi-session corpora, a session that
//! restarts and heals mid-capture, a roster of many small sources around one
//! big one, and oversized, over-one-MiB and trailing-partial records, a pool
//! of 1, 2, 4 or 8 workers or the default size — under its priority scheduler
//! and under a seeded chaos scheduler whose bounds are one byte — leaves
//! exactly what the serial walk leaves: the same ledger bytes in 4 KiB
//! segments and the same segment index, the same closed sources in the same
//! order — as the runtime's `StreamedPreparer` prepared them on emission,
//! with the same hand-off to admission — the same report counters, the same
//! planner cursors, and the same canonical rows once the closed sources are
//! admitted. Failures keep the serial walk's semantics (`faults.rs`).

use std::any::Any;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::{Ledger, SegmentCodec, TailConfig};
use rosetta_store::Store;
use rusqlite::types::Value;
use tempfile::TempDir;

use super::CapturePool;
use crate::closed::tests::{
    HEAL_JOURNAL, HEAL_TRANSCRIPT, RewriteOnce, adapters, claude_user, copy_tree, fixture_roots,
};
use crate::{
    ClosedSource, EarlyToolReport, IngestOptions, Instrument, PipelineError, StreamedPreparer,
    StreamedRuntime, discover, expand_data_root, meta_ingest_inventory_into,
    stream_closed_sources_into, stream_closed_sources_pooled, streamed_setup,
};

/// One way to capture: the serial walk (`workers == 0`), or the pool under
/// its priority scheduler or a seeded chaos scheduler.
#[derive(Debug, Clone, Copy)]
struct Config {
    workers: usize,
    chaos: Option<u64>,
}

const SERIAL: Config = Config {
    workers: 0,
    chaos: None,
};

const MATRIX: [Config; 9] = [
    SERIAL,
    Config {
        workers: 1,
        chaos: None,
    },
    Config {
        workers: 1,
        chaos: Some(11),
    },
    Config {
        workers: 2,
        chaos: None,
    },
    Config {
        workers: 2,
        chaos: Some(23),
    },
    Config {
        workers: 4,
        chaos: None,
    },
    Config {
        workers: 4,
        chaos: Some(37),
    },
    Config {
        workers: 8,
        chaos: None,
    },
    Config {
        workers: 8,
        chaos: Some(53),
    },
];

/// The matrix, plus the default pool size under both schedulers when it is
/// not one of them already.
fn configs() -> Vec<Config> {
    let mut configs = MATRIX.to_vec();
    let default = CapturePool::default_workers();
    if !configs.iter().any(|config| config.workers == default) {
        configs.push(Config {
            workers: default,
            chaos: None,
        });
        configs.push(Config {
            workers: default,
            chaos: Some(97),
        });
    }
    configs
}

/// Everything one capture leaves behind.
#[derive(Debug, PartialEq)]
struct Captured {
    /// The capture's error text, if it failed.
    result: Result<(), String>,
    /// Every planner cursor column, in id order.
    cursors: Vec<Vec<Value>>,
    /// Each closed source as emitted and prepared, in emission order.
    emitted: Vec<String>,
    /// What the preparer hands admission — its counters without its timer,
    /// and the sessions it leaves to be re-ranked — when the capture and the
    /// preparer both finished.
    admission: Option<(EarlyToolReport, Vec<String>)>,
    /// The report's counters; empty when the capture failed.
    report: Vec<u64>,
    /// Every file of the ledger directory — segments and segment index —
    /// in name order.
    ledger: Vec<(String, Vec<u8>)>,
    /// Every canonical table once the closed sources are admitted; empty
    /// when the capture failed.
    canonical: BTreeMap<String, Vec<String>>,
}

fn tiny_batches(records: usize) -> IngestOptions {
    let mut tail = TailConfig::json_lines();
    tail.max_records_per_batch = records;
    IngestOptions {
        tail,
        ..IngestOptions::default()
    }
}

/// Discover, plan metadata into the ledger, publish it to a canonical store
/// and open the streamed writer — fresh serve's steps — then capture
/// `config`'s way into that 4 KiB-segment asynchronous ledger at
/// `ledger_dir`, applying every emission as it arrives.
fn capture_into(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    opts: &IngestOptions,
    config: Config,
    ledger_dir: &Path,
) -> Captured {
    let instrument = Instrument::new();
    let discovery = discover(&[adapter], home, opts, &instrument)
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    // One discovery time for every configuration, so the metadata frame and
    // the runtime rows compare.
    runtime.discovered_at = 1;
    let mut ledger = Ledger::open(ledger_dir)
        .unwrap()
        .with_codec(SegmentCodec::Plain)
        .with_max_segment_bytes(4 * 1024)
        .defer_sync_until_flush()
        .into_async_bounded(64 * 1024);
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let (_, files) = meta_ingest_inventory_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut planner,
        &discovery.inventory,
        discovery.walk_truncated,
        opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    let mut canonical = cold_store();
    for batch in planner.planned_metadata_batches().unwrap() {
        canonical.apply(&batch).unwrap();
    }
    let setup = streamed_setup(&files, &mut planner).unwrap();
    let mut writer =
        StreamedRuntime::open(adapter, &runtime, setup, &mut canonical, &instrument).unwrap();

    // Fresh serve's emission: the preparer first, on the thread that walks
    // (the sequencer, when pooled), then the writer.
    let mut preparer = StreamedPreparer::new(&files);
    let mut emitted = Vec::new();
    let mut emit = |mut sources: Vec<ClosedSource>| {
        preparer.prepare(&mut sources)?;
        emitted.extend(sources.iter().map(|source| format!("{source:?}")));
        writer
            .apply(&mut canonical, sources, &instrument)
            .map(|_| ())
    };
    let captured = catch_unwind(AssertUnwindSafe(|| {
        if config.workers == 0 {
            return stream_closed_sources_into(
                adapter,
                &runtime,
                &discovery.root,
                &mut planner,
                &files,
                opts,
                &instrument,
                Some(&mut ledger),
                &mut emit,
            );
        }
        let pool = match config.chaos {
            Some(seed) => CapturePool::chaos(config.workers, seed),
            None => CapturePool::new(config.workers, super::PoolBudgets::DEFAULT),
        };
        pool.scoped(|| {
            stream_closed_sources_pooled(
                &pool,
                adapter,
                &runtime,
                &discovery.root,
                &mut planner,
                &files,
                opts,
                &instrument,
                Some(&mut ledger),
                &mut emit,
            )
        })
    }))
    .unwrap_or_else(|panic| {
        Err(PipelineError::Config(format!(
            "panicked: {}",
            text(&*panic)
        )))
    });
    ledger.flush().unwrap();
    drop(ledger);

    let (result, report, admission, canonical) = match captured {
        Ok(report) => {
            // Admission and finalization may refuse what capture produced
            // (an abandoned first generation); serve then fails the build.
            let mut admission = None;
            let admitted = preparer
                .finish()
                .map_err(|error| error.to_string())
                .and_then(|prepared| {
                    admission = Some((
                        EarlyToolReport {
                            nanos: 0,
                            ..prepared.report()
                        },
                        prepared.sessions_to_renumber().to_vec(),
                    ));
                    writer
                        .admit(&mut canonical, &instrument, &prepared)
                        .map_err(|error| error.to_string())
                })
                .and_then(|_| {
                    canonical
                        .finish_bulk_load()
                        .map_err(|error| format!("{error:?}"))
                });
            let canonical = match admitted {
                Ok(_) => dump(&canonical),
                Err(error) => BTreeMap::from([("<refused>".to_string(), vec![error])]),
            };
            let counters = vec![
                report.files as u64,
                report.files_missing as u64,
                report.batches,
                report.records,
                report.payload_bytes,
                report.oversized,
                report.parsed,
                report.unknown,
                report.hard_errors,
                report.generations_restarted,
                report.files_audited,
                report.conflicts,
                report.retries_exhausted,
                report.files_incomplete,
                report.files_paused,
                report.sessions_indexed,
                report.siblings_healed,
            ];
            (Ok(()), counters, admission, canonical)
        }
        Err(error) => (Err(error.to_string()), Vec::new(), None, BTreeMap::new()),
    };
    Captured {
        result,
        cursors: rows(&planner, "SELECT * FROM ingest_cursor ORDER BY id"),
        emitted,
        admission,
        report,
        ledger: ledger_files(ledger_dir),
        canonical,
    }
}

fn capture(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    opts: &IngestOptions,
    config: Config,
) -> Captured {
    let ledger = TempDir::new().unwrap();
    capture_into(adapter, home, opts, config, ledger.path())
}

fn text(panic: &(dyn Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

fn cold_store() -> Store {
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.use_precomputed_bulk_ordinals().unwrap();
    store
}

fn rows(store: &Store, sql: &str) -> Vec<Vec<Value>> {
    store
        .conn()
        .prepare(sql)
        .unwrap()
        .query_map([], |row| {
            (0..row.as_ref().column_count())
                .map(|index| row.get::<_, Value>(index))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn ledger_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// Every canonical table, surrogate ids mapped to stable identities (the
/// frozen digest's normalization), rows sorted.
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
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut out = BTreeMap::new();
    for table in tables {
        let mut statement = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let columns: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut table_rows = statement
            .query_map([], |row| {
                let mut fields = Vec::new();
                for (index, column) in columns.iter().enumerate() {
                    if ((table == "message" || table == "ingest_cursor") && column == "id")
                        || (table == "projection_state" && column == "projection_epoch")
                    {
                        continue;
                    }
                    let value: Value = row.get(index)?;
                    let shown = match (&value, column.as_str()) {
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
                    fields.push(format!("{column}={shown}"));
                }
                Ok(fields.join(" | "))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        table_rows.sort();
        out.insert(table, table_rows);
    }
    out
}

/// Two dumps are equal; otherwise name the first table that differs and a
/// few rows only one side has.
fn assert_same_dump(
    label: &str,
    expected: &BTreeMap<String, Vec<String>>,
    actual: &BTreeMap<String, Vec<String>>,
) {
    assert_eq!(
        expected.keys().collect::<Vec<_>>(),
        actual.keys().collect::<Vec<_>>(),
        "{label}: table sets differ"
    );
    for (table, rows) in expected {
        let other = &actual[table];
        if rows != other {
            let missing: Vec<_> = rows
                .iter()
                .filter(|row| !other.contains(row))
                .take(3)
                .collect();
            let extra: Vec<_> = other
                .iter()
                .filter(|row| !rows.contains(row))
                .take(3)
                .collect();
            panic!(
                "{label}: table {table} differs ({} expected rows, {} actual)\n\
                 only expected: {missing:#?}\nonly actual: {extra:#?}",
                rows.len(),
                other.len()
            );
        }
    }
}

/// Capture `config`'s way and assert it leaves exactly what the serial walk
/// left.
fn assert_matches(label: &str, serial: &Captured, pooled: &Captured, config: Config) {
    let label = format!("{label} {config:?}");
    assert_eq!(pooled.result, serial.result, "{label}: result");
    assert_eq!(pooled.report, serial.report, "{label}: report");
    assert_eq!(pooled.cursors, serial.cursors, "{label}: planner cursors");
    assert_eq!(pooled.emitted, serial.emitted, "{label}: closed sources");
    assert_eq!(
        pooled.admission, serial.admission,
        "{label}: preparer hand-off"
    );
    assert_eq!(
        pooled
            .ledger
            .iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        serial
            .ledger
            .iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        "{label}: ledger files"
    );
    for ((name, pooled), (_, serial)) in pooled.ledger.iter().zip(&serial.ledger) {
        assert!(pooled == serial, "{label}: ledger file {name} differs");
    }
    assert_same_dump(&label, &serial.canonical, &pooled.canonical);
}

/// Every configuration against the serial walk, whatever the serial walk's
/// result; returns the serial capture.
fn assert_every_config_like_serial(label: &str, run: impl Fn(Config) -> Captured) -> Captured {
    let serial = run(SERIAL);
    for config in &configs()[1..] {
        assert_matches(label, &serial, &run(*config), *config);
    }
    serial
}

/// [`assert_every_config_like_serial`] for a capture that succeeds.
fn assert_every_config(label: &str, run: impl Fn(Config) -> Captured) -> Captured {
    let serial = assert_every_config_like_serial(label, run);
    assert!(serial.result.is_ok(), "{label}: {:?}", serial.result);
    serial
}

#[test]
fn the_pool_matches_the_serial_walk_on_every_fixture() {
    let mut scenarios = 0;
    for adapter in adapters() {
        for (scenario, data) in fixture_roots(adapter.kind()) {
            let home = TempDir::new().unwrap();
            copy_tree(
                &data,
                &expand_data_root(&adapter.layout().data_root, home.path()),
            );
            for (batching, opts) in [
                ("whole batches", IngestOptions::default()),
                ("2-record batches", tiny_batches(2)),
            ] {
                let label = format!("{} {scenario} {batching}", adapter.kind());
                assert_every_config(&label, |config| {
                    capture(adapter, home.path(), &opts, config)
                });
            }
            scenarios += 1;
        }
    }
    assert!(scenarios >= 27, "only {scenarios} fixture scenarios");
}

#[test]
fn the_pool_matches_the_serial_walk_on_generated_corpora() {
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
        generate(&CorpusSpec::new(agent, 12, 2_000, seed), &root).unwrap();
        for (batching, opts) in [
            ("whole batches", IngestOptions::default()),
            ("16-record batches", tiny_batches(16)),
        ] {
            let label = format!("{} generated {batching}", adapter.kind());
            let serial = assert_every_config(&label, |config| {
                capture(adapter, home.path(), &opts, config)
            });
            assert!(serial.report[3] > 1_000, "{label}: {:?}", serial.report);
            // The prepared sources compared above carry resolved tool rows.
            let (early, _) = serial.admission.as_ref().expect("the preparer finished");
            assert!(
                early.sessions > 0 && early.tool_rows > 0,
                "{label}: {early:?}"
            );
        }
    }
}

/// The restart-and-heal home of the eager-cursor tests: the transcript is
/// rewritten after its first batch, restarts, and its sibling journal heals
/// before either is emitted.
#[test]
fn the_pool_matches_the_serial_walk_across_a_restart_and_heal() {
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
    let transcript = root.join(HEAL_TRANSCRIPT);
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let serial = assert_every_config("restart", |config| {
        std::fs::write(&transcript, lines("old")).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&transcript)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_790_000_000))
            .unwrap();
        let opts = IngestOptions {
            progress: Some(Arc::new(RewriteOnce {
                root: root.clone(),
                target: HEAL_TRANSCRIPT.into(),
                replacement: lines("new"),
                done: AtomicBool::new(false),
            })),
            ..tiny_batches(2)
        };
        capture(&ClaudeCodeAdapter, home.path(), &opts, config)
    });
    // Both sources restarted or healed: the transcript restarts, the heal
    // replays its sibling into a replacement generation.
    assert_eq!(
        (serial.report[9], serial.report[16]),
        (2, 1),
        "{:?}",
        serial.report
    );
}

fn claude_line(session: &str, uuid: &str, content: &str) -> String {
    serde_json::json!({
        "parentUuid": null,
        "isSidechain": false,
        "type": "user",
        "message": {"role": "user", "content": content},
        "uuid": uuid,
        "timestamp": "2026-08-25T02:35:19.054Z",
        "userType": "external",
        "cwd": "/w/pool",
        "sessionId": session,
        "version": "2.1.241",
        "gitBranch": "main"
    })
    .to_string()
}

/// One Claude transcript per `(session, records)`, each record `lines`
/// builds, under one project.
fn claude_home(sessions: &[(String, Vec<String>)], partial_tail: Option<&str>) -> TempDir {
    let home = TempDir::new().unwrap();
    let project = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path())
        .join("projects/-w-pool");
    std::fs::create_dir_all(&project).unwrap();
    for (session, records) in sessions {
        let mut body = records.join("\n") + "\n";
        if let Some(tail) = partial_tail.filter(|_| session.ends_with("partial")) {
            body.push_str(tail);
        }
        std::fs::write(project.join(format!("{session}.jsonl")), body).unwrap();
    }
    home
}

fn session_records(session: &str, count: usize, content: &str) -> Vec<String> {
    (0..count)
        .map(|index| claude_line(session, &format!("{session}-{index}"), content))
        .collect()
}

#[test]
fn the_pool_matches_the_serial_walk_on_many_small_sources_and_one_big() {
    let sessions: Vec<(String, Vec<String>)> = (0..40)
        .map(|index| {
            let session = format!("s{index:02}");
            let count = if index == 20 { 3_000 } else { 3 };
            let records = session_records(&session, count, &format!("{session} says hello"));
            (session, records)
        })
        .collect();
    let home = claude_home(&sessions, None);
    for (batching, opts) in [
        ("whole batches", IngestOptions::default()),
        ("16-record batches", tiny_batches(16)),
    ] {
        let label = format!("many small, one big, {batching}");
        let serial = assert_every_config(&label, |config| {
            capture(&ClaudeCodeAdapter, home.path(), &opts, config)
        });
        assert_eq!(serial.report[0], 40, "{label}: {:?}", serial.report);
    }
}

#[test]
fn the_pool_matches_the_serial_walk_on_oversized_large_and_partial_records() {
    let large = "x".repeat(1536 * 1024);
    let oversized = "y".repeat(3 * 1024 * 1024);
    let with = |session: &str, middle: &str| {
        let mut records = session_records(session, 2, "before");
        records.push(claude_line(session, &format!("{session}-big"), middle));
        records.extend(
            (0..2).map(|index| claude_line(session, &format!("{session}-after-{index}"), "after")),
        );
        records
    };
    let mut sessions = vec![
        ("a-large".to_string(), with("a-large", &large)),
        (
            "d-plain".to_string(),
            session_records("d-plain", 4, "plain"),
        ),
    ];
    let mut tail = TailConfig::json_lines();
    tail.max_record_bytes = 2 * 1024 * 1024;
    tail.max_records_per_batch = 2;
    let opts = IngestOptions {
        tail,
        ..IngestOptions::default()
    };
    let home = claude_home(&sessions, None);
    let serial = assert_every_config("large record", |config| {
        capture(&ClaudeCodeAdapter, home.path(), &opts, config)
    });
    assert!(
        serial
            .ledger
            .iter()
            .map(|(_, bytes)| bytes.len())
            .sum::<usize>()
            > large.len(),
        "the large record reached the ledger"
    );
    assert!(
        serial.canonical.contains_key("message"),
        "{:?}",
        serial.canonical.keys()
    );

    // An oversized record leaves its first generation raw-incomplete, so it
    // is abandoned, and admission refuses the runtime, as serve does.
    sessions.insert(
        1,
        ("b-oversized".to_string(), with("b-oversized", &oversized)),
    );
    let home = claude_home(&sessions, None);
    let serial = assert_every_config("oversized record", |config| {
        capture(&ClaudeCodeAdapter, home.path(), &opts, config)
    });
    assert_eq!(serial.report[5], 1, "{:?}", serial.report);
    assert!(
        serial.canonical.contains_key("<refused>"),
        "{:?}",
        serial.canonical.keys()
    );

    // A transcript that ends inside a record stops without a terminal
    // generation, which fails the streamed build closed at that source —
    // the serial walk's behaviour, which the pool keeps, prefix and all.
    sessions.insert(
        2,
        (
            "c-partial".to_string(),
            session_records("c-partial", 3, "complete"),
        ),
    );
    let home = claude_home(
        &sessions,
        Some(r#"{"type":"user","message":{"role":"user","content":"cut"#),
    );
    let serial = assert_every_config_like_serial("partial record", |config| {
        capture(&ClaudeCodeAdapter, home.path(), &opts, config)
    });
    let error = serial.result.unwrap_err();
    assert!(
        error.contains("c-partial.jsonl stopped without a terminal generation"),
        "{error}"
    );
}

mod faults;
