//! A Claude response can continue from a workflow file into its ordinary
//! subagent file. Both sources belong to one session and carry native slots.
use std::{fs, io::Write, path::Path};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_core::{ids, rows::RuntimeRow};
use rosetta_pipeline::{
    IngestOptions, Instrument, RebuildOptions, StreamedPreparer, StreamedRuntime,
    apply_prepared_runtime, meta_ingest_into, prepare_runtime, rebuild_runtime_with_options,
    stream_backfill_into, stream_closed_sources_into, streamed_setup,
};
use rosetta_store::Store;
use serde_json::{Value, json};

fn record(uuid: &str, index: u32, block: Value) -> Value {
    json!({
        "type": "assistant", "uuid": uuid, "parentUuid": null,
        "sessionId": "parent", "agentId": "child", "isSidechain": true,
        "timestamp": "2026-09-28T00:00:00Z", "version": "2.1.283",
        "cwd": "/w/project", "gitBranch": "main", "userType": "external",
        "requestId": "req-split", "apiBlockIndex": index,
        "message": {
            "id": "msg-split", "role": "assistant", "type": "message",
            "model": "claude-opus-5", "content": [block],
            "stop_reason": "end_turn", "stop_sequence": null
        }
    })
}

fn write(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = records.iter().map(|row| format!("{row}\n")).collect();
    fs::write(path, text).unwrap();
}

fn rows(store: &Store) -> Vec<(u32, String, String)> {
    store
        .conn()
        .prepare(
            "SELECT block_index, type, uuid FROM message
             WHERE response_id = 'msg-split' ORDER BY block_index",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn split_workflow_response_survives_cold_live_resume_and_ledger_replay() {
    for streamed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("claude");
        let child = root.join("projects/p/parent/subagents/agent-child.jsonl");
        let workflow = root.join("projects/p/parent/subagents/workflows/wf-run/agent-child.jsonl");
        // The alphabetically first source begins at block 2. Hidden block 0
        // and prose block 1 live in the other file; no file-local counter can
        // reconstruct their shared response positions.
        write(
            &child,
            &[record(
                "tool",
                2,
                json!({
                    "type": "tool_use", "id": "toolu-split", "name": "Bash",
                    "input": {"command": "true"}
                }),
            )],
        );
        write(
            &workflow,
            &[
                record(
                    "hidden",
                    0,
                    json!({"type": "thinking", "thinking": "", "signature": "sig"}),
                ),
                record("prose", 1, json!({"type": "text", "text": "Review ready."})),
            ],
        );
        let runtime = RuntimeRow {
            id: ids::runtime_id("claude-code", root.to_str().unwrap()),
            kind: "claude-code".into(),
            data_root: root.to_string_lossy().into_owned(),
            enabled: true,
            account: None,
            config: None,
            skills: None,
            versions_seen: None,
            discovered_at: 1,
            label: None,
            provenance: Default::default(),
        };
        let options = IngestOptions {
            audit_files_per_run: 0,
            ..Default::default()
        };
        let instrument = Instrument::new();
        let ledger_path = temp.path().join("ledger");
        let mut ledger = rosetta_capture::Ledger::open(&ledger_path).unwrap();
        let mut store = if streamed {
            Store::open_in_memory_for_rebuild().unwrap()
        } else {
            Store::open_in_memory().unwrap()
        };
        let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
        let (_, files) = meta_ingest_into(
            &ClaudeCodeAdapter,
            &runtime,
            &root,
            if streamed { &mut planner } else { &mut store },
            &options,
            &instrument,
            Some(&mut ledger),
        )
        .unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].session_id, files[1].session_id);
        if streamed {
            store.use_precomputed_bulk_ordinals().unwrap();
            for batch in planner.planned_metadata_batches().unwrap() {
                store.apply(&batch).unwrap();
            }
            let setup = streamed_setup(&files, &mut planner).unwrap();
            let mut writer =
                StreamedRuntime::open(&ClaudeCodeAdapter, &runtime, setup, &mut store, &instrument)
                    .unwrap();
            let mut preparer = StreamedPreparer::new(&files);
            stream_closed_sources_into(
                &ClaudeCodeAdapter,
                &runtime,
                &root,
                &mut planner,
                &files,
                &options,
                &instrument,
                Some(&mut ledger),
                &mut |mut sources| {
                    preparer.prepare(&mut sources)?;
                    writer.apply(&mut store, sources, &instrument)?;
                    Ok(())
                },
            )
            .unwrap();
            writer
                .admit(&mut store, &instrument, &preparer.finish().unwrap())
                .unwrap();
            store.finish_bulk_load().unwrap();
        } else {
            stream_backfill_into(
                &ClaudeCodeAdapter,
                &runtime,
                &root,
                &mut store,
                &files,
                &options,
                &instrument,
                Some(&mut ledger),
            )
            .unwrap();
        }
        let mut expected = vec![
            (1, "agent".into(), "prose".into()),
            (2, "tool_use".into(), "tool".into()),
        ];
        assert_eq!(rows(&store), expected);

        // Append after the source checkpoint: its first recorded block was
        // native 2, so the continuation must keep its native position 3.
        let later = record("later", 3, json!({"type": "text", "text": "Finished."}));
        writeln!(
            fs::OpenOptions::new().append(true).open(&child).unwrap(),
            "{later}"
        )
        .unwrap();
        let (_, files) = meta_ingest_into(
            &ClaudeCodeAdapter,
            &runtime,
            &root,
            &mut store,
            &options,
            &instrument,
            Some(&mut ledger),
        )
        .unwrap();
        stream_backfill_into(
            &ClaudeCodeAdapter,
            &runtime,
            &root,
            &mut store,
            &files,
            &options,
            &instrument,
            Some(&mut ledger),
        )
        .unwrap();
        expected.push((3, "agent".into(), "later".into()));
        assert_eq!(rows(&store), expected);
        ledger.flush().unwrap();
        fs::remove_dir_all(&root).unwrap();

        let mut replay = Store::open_in_memory_for_rebuild().unwrap();
        let cache = temp.path().join("election.json");
        rebuild_runtime_with_options(
            &ClaudeCodeAdapter,
            &runtime,
            &ledger_path,
            &mut replay,
            &instrument,
            RebuildOptions {
                chunk_ops: 1,
                election_cache: Some(cache.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rows(&replay), expected);
        let prepared = prepare_runtime(
            &ClaudeCodeAdapter,
            &runtime,
            &ledger_path,
            &instrument,
            RebuildOptions {
                election_cache: Some(cache),
                ..Default::default()
            },
        )
        .unwrap();
        let mut retained = Store::open_in_memory_for_rebuild().unwrap();
        apply_prepared_runtime(
            &ClaudeCodeAdapter,
            &runtime,
            prepared,
            &mut retained,
            &instrument,
        )
        .unwrap();
        retained.finish_bulk_load().unwrap();
        assert_eq!(rows(&retained), expected);
    }
}
