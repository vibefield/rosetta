//! A record over the projection limit, captured whole: every worker count and
//! both ledger codecs publish it as one drift fact, and a rebuild from the raw
//! ledger alone — streaming or prepared replay — reproduces that projection.

use std::collections::BTreeMap;
use std::path::Path;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{LedgerEntry, scan_ledger};
use rosetta_core::rows::RuntimeRow;
use rosetta_store::Store;
use tempfile::TempDir;

use super::{
    SERIAL, assert_every_config, assert_same_dump, capture, capture_into, claude_home, claude_line,
    dump, session_records, stream_state, tiny_batches,
};
use crate::{
    IngestOptions, Instrument, RebuildOptions, apply_prepared_runtime, discover, prepare_runtime,
    rebuild_runtime, rebuild_runtime_with_options,
};

/// Two Claude transcripts, one holding a record just over the projection
/// limit between four ordinary ones.
fn huge_record_home() -> TempDir {
    let huge = "q".repeat(crate::MAX_PROJECTED_RECORD_BYTES + 1024);
    let mut records = session_records("b-huge", 2, "before");
    records.push(claude_line("b-huge", "b-huge-big", &huge));
    records.extend(
        (0..2).map(|index| claude_line("b-huge", &format!("b-huge-after-{index}"), "after")),
    );
    claude_home(
        &[
            (
                "a-plain".to_string(),
                session_records("a-plain", 3, "plain"),
            ),
            ("b-huge".to_string(), records),
        ],
        None,
    )
}

/// A record over the projection limit (but inside the capture limit) is
/// ledgered whole and projects one drift fact instead of being normalized:
/// its generation stays clean, seals and activates, the session is Indexed,
/// and the build publishes — identically for every worker count and for plain
/// and zstd-frames ledgers.
#[test]
fn the_pool_matches_the_serial_walk_on_a_record_over_the_projection_limit() {
    let home = huge_record_home();
    let opts = tiny_batches(2);
    let serial = assert_every_config("over the projection limit", |config| {
        capture(&ClaudeCodeAdapter, home.path(), &opts, config)
    });
    // Oversized, hard errors, files incomplete, quarantined.
    assert_eq!(
        (
            serial.report[5],
            serial.report[8],
            serial.report[13],
            serial.report[17]
        ),
        (0, 0, 0, 1),
        "{:?}",
        serial.report
    );
    assert!(
        !serial.canonical.contains_key("<refused>"),
        "{:?}",
        serial.canonical["<refused>"]
    );
    assert_eq!(stream_state(&serial.canonical, "b-huge"), "indexed");
    let drift = serial.canonical["drift_agg"]
        .iter()
        .filter(|row| row.contains("error_code=Text(\"over_projection_limit\")"))
        .collect::<Vec<_>>();
    assert_eq!(drift.len(), 1, "{:?}", serial.canonical["drift_agg"]);
    assert!(drift[0].contains("b-huge.jsonl"), "{}", drift[0]);
    let contents = |content: &str| {
        let field = format!("content=Text(\"{content}\")");
        serial.canonical["message"]
            .iter()
            .filter(|row| row.split(" | ").any(|column| column == field))
            .count()
    };
    assert_eq!((contents("before"), contents("after")), (2, 2));
    assert!(
        serial.canonical["message"]
            .iter()
            .all(|row| row.len() < crate::MAX_PROJECTED_RECORD_BYTES),
        "the record itself is never projected"
    );

    // The record is in the ledger whole, under a clean seal.
    let ledger = TempDir::new().unwrap();
    capture_into(
        &ClaudeCodeAdapter,
        home.path(),
        &opts,
        SERIAL,
        ledger.path(),
    );
    let entries = scan_ledger(ledger.path()).unwrap().entries;
    let huge_records = entries
        .iter()
        .filter(|entry| matches!(
            entry,
            LedgerEntry::Raw { payload, .. } if payload.len() > crate::MAX_PROJECTED_RECORD_BYTES
        ))
        .count();
    assert_eq!(huge_records, 1);
    let seals: Vec<_> = entries
        .iter()
        .filter_map(|entry| match entry {
            LedgerEntry::GenerationComplete {
                source,
                generation,
                normalized_clean,
                ..
            } if source.relative_path.ends_with("b-huge.jsonl") => {
                Some((*generation, *normalized_clean))
            }
            _ => None,
        })
        .collect();
    assert_eq!(seals, [(1, true)]);
}

/// The runtime a capture of `home` registers, as `capture_into` pins it.
fn runtime_of(adapter: &'static dyn AgentAdapter, home: &Path, opts: &IngestOptions) -> RuntimeRow {
    let mut runtime = discover(&[adapter], home, opts, &Instrument::new())
        .unwrap()
        .pop()
        .unwrap()
        .runtime;
    runtime.enabled = true;
    runtime.discovered_at = 1;
    runtime
}

/// Only the tables a reader sees: cursors keep checkpoints only the capture
/// writes, and the projection's identity is regenerated every build.
fn reader_tables(dump: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    dump.iter()
        .filter(|(table, _)| !matches!(table.as_str(), "ingest_cursor" | "projection_state"))
        .map(|(table, rows)| (table.clone(), rows.clone()))
        .collect()
}

/// A rebuild from the raw ledger alone — the streaming replay, and the
/// prepared replay behind an election cache — reproduces the projection the
/// capture published, the quarantined record's drift fact included.
#[test]
fn a_rebuild_from_the_ledger_reproduces_a_record_over_the_projection_limit() {
    let home = huge_record_home();
    let opts = tiny_batches(2);
    let ledger = TempDir::new().unwrap();
    let built = capture_into(
        &ClaudeCodeAdapter,
        home.path(),
        &opts,
        SERIAL,
        ledger.path(),
    );
    assert!(
        built.canonical.contains_key("drift_agg"),
        "{:?}",
        built.canonical.keys()
    );
    let expected = reader_tables(&built.canonical);
    let runtime = runtime_of(&ClaudeCodeAdapter, home.path(), &opts);
    let instrument = Instrument::new();

    let mut streamed = Store::open_in_memory_for_rebuild().unwrap();
    let report = rebuild_runtime(
        &ClaudeCodeAdapter,
        &runtime,
        ledger.path(),
        &mut streamed,
        &instrument,
    )
    .unwrap();
    assert_eq!((report.quarantined, report.hard_errors), (1, 0));
    streamed.finish_bulk_load().unwrap();
    assert_same_dump(
        "streaming replay",
        &expected,
        &reader_tables(&dump(&streamed)),
    );

    let cache = ledger.path().join("election.json");
    let mut elected = Store::open_in_memory_for_rebuild().unwrap();
    rebuild_runtime_with_options(
        &ClaudeCodeAdapter,
        &runtime,
        ledger.path(),
        &mut elected,
        &instrument,
        RebuildOptions {
            election_only: true,
            election_cache: Some(cache.clone()),
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    let prepared = prepare_runtime(
        &ClaudeCodeAdapter,
        &runtime,
        ledger.path(),
        &instrument,
        RebuildOptions {
            election_cache: Some(cache),
            ..RebuildOptions::default()
        },
    )
    .unwrap();
    assert_eq!(prepared.report().quarantined, 1);
    let mut applied = Store::open_in_memory_for_rebuild().unwrap();
    apply_prepared_runtime(
        &ClaudeCodeAdapter,
        &runtime,
        prepared,
        &mut applied,
        &instrument,
    )
    .unwrap();
    applied.finish_bulk_load().unwrap();
    assert_same_dump(
        "prepared replay",
        &expected,
        &reader_tables(&dump(&applied)),
    );
}
