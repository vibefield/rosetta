//! A resumed private build withholds readiness. Its staged-ledger replay and
//! its serial catch-up compute exactly the stream states their committing
//! forms set, commit none of them — every session stays Pending, so no read
//! route can show its rows before reconciliation and finalization — and once
//! the caller commits them after finalization the projection equals the one
//! the committing forms build.

use std::collections::BTreeMap;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::Ledger;
use rosetta_core::rows::StreamState;
use rosetta_pipeline::{
    IngestOptions, Instrument, RebuildOptions, discover, expand_data_root, meta_ingest_into,
    rebuild_runtime_with_options, stream_backfill_into, stream_backfill_withholding_health,
};
use rosetta_store::Store;
use tempfile::TempDir;

/// Every session's stream state, Pending included.
fn states(store: &Store) -> BTreeMap<String, String> {
    store
        .conn()
        .prepare("SELECT id, stream_state FROM session")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The decided sessions only, as a state map would hold them.
fn decided(store: &Store) -> BTreeMap<String, StreamState> {
    states(store)
        .into_iter()
        .filter_map(|(id, state)| match state.as_str() {
            "indexed" => Some((id, StreamState::Indexed)),
            "streaming" => Some((id, StreamState::Streaming)),
            _ => None,
        })
        .collect()
}

fn all_pending(store: &Store) -> bool {
    states(store).values().all(|state| state == "pending")
}

/// The timeline and tool rows readers see, in timeline order.
fn timeline(store: &Store) -> Vec<String> {
    store
        .conn()
        .prepare(
            "SELECT session_id, ordinal, timeline_id, type, content, tool_correlation_key,
                    tool_name, tool_status, tool_detail
             FROM message ORDER BY session_id, ordinal",
        )
        .unwrap()
        .query_map([], |row| {
            (0..9)
                .map(|index| row.get::<_, rusqlite::types::Value>(index))
                .collect::<Result<Vec<_>, _>>()
                .map(|values| format!("{values:?}"))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn replay_and_catch_up_withhold_exactly_the_states_they_would_commit() {
    let adapter = &ClaudeCodeAdapter;
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    generate(&CorpusSpec::new(Agent::ClaudeCode, 6, 260, 42), &root).unwrap();
    let opts = IngestOptions {
        audit_files_per_run: 0,
        private_build: true,
        ..IngestOptions::default()
    };
    let instrument = Instrument::new();
    let mut runtime = discover(&[adapter], home.path(), &opts, &instrument).unwrap()[0]
        .runtime
        .clone();
    runtime.enabled = true;

    // The interrupted start: a private build that ledgered every source.
    let ledger_dir = home.path().join("ledger/claude-code");
    let mut ledger = Ledger::open(&ledger_dir).unwrap();
    let mut captured = Store::open_in_memory_for_rebuild().unwrap();
    let (_, files) = meta_ingest_into(
        adapter,
        &runtime,
        &root,
        &mut captured,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    stream_backfill_into(
        adapter,
        &runtime,
        &root,
        &mut captured,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    // The resume's replay, committing and withholding.
    let replay = |withhold_session_health| {
        let mut store = Store::open_in_memory_for_rebuild().unwrap();
        let report = rebuild_runtime_with_options(
            adapter,
            &runtime,
            &ledger_dir,
            &mut store,
            &Instrument::new(),
            RebuildOptions {
                flush_reconciliation: false,
                allow_unsealed_sources: true,
                withhold_session_health,
                ..RebuildOptions::default()
            },
        )
        .unwrap();
        (store, report)
    };
    let (mut committed, committed_report) = replay(false);
    let (mut withheld, withheld_report) = replay(true);
    assert!(committed_report.session_health.is_empty());
    let replayed = withheld_report.session_health;
    assert!(replayed.len() >= 6, "{replayed:?}");
    assert_eq!(replayed, decided(&committed));
    assert!(all_pending(&withheld), "{:?}", states(&withheld));
    assert_eq!(timeline(&withheld), timeline(&committed));

    // The resume's serial catch-up over the same roster.
    stream_backfill_into(
        adapter,
        &runtime,
        &root,
        &mut committed,
        &files,
        &opts,
        &Instrument::new(),
        None,
    )
    .unwrap();
    let mut caught_up = BTreeMap::new();
    let report = stream_backfill_withholding_health(
        adapter,
        &runtime,
        &root,
        &mut withheld,
        &files,
        &opts,
        &Instrument::new(),
        None,
        &mut caught_up,
    )
    .unwrap();
    assert_eq!(report.sessions_indexed, 0);
    assert!(all_pending(&withheld), "{:?}", states(&withheld));
    let roster_sessions = files
        .iter()
        .map(|file| file.session_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        caught_up
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        roster_sessions
    );
    let committed_states = decided(&committed);
    for (session, state) in &caught_up {
        assert_eq!(committed_states.get(session), Some(state), "{session}");
    }

    // Finalization never consults readiness: committing the withheld states
    // after it yields the committing build's projection.
    committed.finish_bulk_load().unwrap();
    withheld.finish_bulk_load().unwrap();
    assert!(all_pending(&withheld));
    let mut merged = replayed;
    merged.extend(caught_up);
    withheld
        .set_session_stream_states(&merged.into_iter().collect::<Vec<_>>())
        .unwrap();
    assert_eq!(states(&withheld), states(&committed));
    assert_eq!(timeline(&withheld), timeline(&committed));
    assert_eq!(timeline(&withheld), {
        captured.finish_bulk_load().unwrap();
        timeline(&captured)
    });
    let tool_rows: i64 = withheld
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM message WHERE type = 'tool_use' AND tool_status IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(tool_rows > 0, "the corpus exercises reconciled tool rows");
}

/// A staged ledger may journal readiness itself: an older binary's resume
/// re-skimmed after committing states and journaled them in its metadata
/// batch. Replaying that batch must not make a session readable either.
#[test]
fn journaled_readiness_is_withheld_like_replays_own() {
    let adapter = &ClaudeCodeAdapter;
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    generate(&CorpusSpec::new(Agent::ClaudeCode, 4, 160, 9), &root).unwrap();
    let opts = IngestOptions {
        audit_files_per_run: 0,
        private_build: true,
        ..IngestOptions::default()
    };
    let instrument = Instrument::new();
    let mut runtime = discover(&[adapter], home.path(), &opts, &instrument).unwrap()[0]
        .runtime
        .clone();
    runtime.enabled = true;
    let ledger_dir = home.path().join("ledger/claude-code");
    let mut ledger = Ledger::open(&ledger_dir).unwrap();
    let mut captured = Store::open_in_memory_for_rebuild().unwrap();
    let (_, files) = meta_ingest_into(
        adapter,
        &runtime,
        &root,
        &mut captured,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    stream_backfill_into(
        adapter,
        &runtime,
        &root,
        &mut captured,
        &files,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    // The re-skim copies each committed state into the batch it journals.
    meta_ingest_into(
        adapter,
        &runtime,
        &root,
        &mut captured,
        &opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    ledger.flush().unwrap();
    drop(ledger);
    assert!(
        !decided(&captured).is_empty(),
        "the journaled batch carries readiness"
    );

    let replay = |withhold_session_health| {
        let mut store = Store::open_in_memory_for_rebuild().unwrap();
        let report = rebuild_runtime_with_options(
            adapter,
            &runtime,
            &ledger_dir,
            &mut store,
            &Instrument::new(),
            RebuildOptions {
                flush_reconciliation: false,
                allow_unsealed_sources: true,
                withhold_session_health,
                ..RebuildOptions::default()
            },
        )
        .unwrap();
        (store, report)
    };
    let (committed, _) = replay(false);
    let (withheld, report) = replay(true);
    assert!(all_pending(&withheld), "{:?}", states(&withheld));
    assert_eq!(report.session_health, decided(&committed));
}
