//! Cross-crate integration: the adapters' committed golden-ops snapshots must
//! apply cleanly to the real projection. This is the spine test — if an
//! adapter emits an op the store can't absorb, it fails here, not in Phase 4.

use std::fs;
use std::path::PathBuf;

use rosetta_core::IngestOp;
use rosetta_core::rows::{RuntimeRow, SessionRole, SessionRow, StreamState};
use rosetta_store::{Batch, Store, queries};

fn golden_dir(adapter: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../adapters")
        .join(adapter)
        .join("tests/golden")
}

/// The two adapters chose different snapshot shapes: codex commits a flat
/// `Vec<IngestOp>`, claude-code wraps ops per transcript file. Accept both.
fn deserialize_golden(path: &std::path::Path, text: &str) -> Vec<IngestOp> {
    #[derive(serde::Deserialize)]
    struct PerTranscript {
        ops: Vec<IngestOp>,
    }
    if let Ok(ops) = serde_json::from_str::<Vec<IngestOp>>(text) {
        return ops;
    }
    match serde_json::from_str::<Vec<PerTranscript>>(text) {
        Ok(wrapped) => wrapped.into_iter().flat_map(|t| t.ops).collect(),
        Err(err) => panic!(
            "{} does not deserialize either shape: {err}",
            path.display()
        ),
    }
}

fn collect_golden_files(adapter: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![golden_dir(adapter)];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Golden snapshots hold stream-phase ops only; runtime and session rows are
/// the meta phase's job. Synthesize them the way the pipeline will.
fn scaffolding(ops: &[IngestOp], runtime_id: &str, kind: &str) -> Vec<IngestOp> {
    let mut session_ids: Vec<String> = ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::Message(row) => Some(row.session_id.clone()),
            IngestOp::ToolUpdate { session_id, .. } | IngestOp::Usage { session_id, .. } => {
                Some(session_id.clone())
            }
            IngestOp::WorkspaceObs(row) => Some(row.session_id.clone()),
            IngestOp::FileEdit(row) => Some(row.session_id.clone()),
            IngestOp::Plan(row) => Some(row.session_id.clone()),
            IngestOp::Task(row) => Some(row.session_id.clone()),
            IngestOp::GitCommit(row) => Some(row.session_id.clone()),
            _ => None,
        })
        .collect();
    session_ids.sort();
    session_ids.dedup();

    // Drift facts and SessionMeta rows carry the runtime id the adapter's
    // golden test minted — every distinct one needs an agent_runtime row.
    let mut runtime_ids: Vec<String> = ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::Drift(fact) => Some(fact.runtime_id.clone()),
            IngestOp::SessionMeta(row) => Some(row.runtime_id.clone()),
            _ => None,
        })
        .chain(std::iter::once(runtime_id.to_string()))
        .collect();
    runtime_ids.sort();
    runtime_ids.dedup();

    let mut out: Vec<IngestOp> = runtime_ids
        .into_iter()
        .map(|id| {
            IngestOp::Runtime(RuntimeRow {
                id,
                kind: kind.into(),
                data_root: format!("~/.{kind}"),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 0,
            })
        })
        .collect();
    for session_id in session_ids {
        out.push(IngestOp::SessionMeta(SessionRow {
            id: session_id.clone(),
            native_session_id: session_id.clone(),
            runtime_id: runtime_id.into(),
            project_id: None,
            parent_session_id: None,
            root_session_id: session_id,
            role: SessionRole::Main,
            title: None,
            summary: None,
            started_at: 0,
            updated_at: 0,
            stream_state: StreamState::Streaming,
        }));
    }
    out
}

fn apply_adapter_goldens(adapter: &str, kind: &str) -> (u64, u64, u64) {
    let files = collect_golden_files(adapter);
    assert!(!files.is_empty(), "no golden snapshots found for {adapter}");

    let mut store = Store::open_in_memory().unwrap();
    let runtime_id = rosetta_core::ids::runtime_id(kind, &format!("~/.{kind}"));
    let (mut inserted, mut merged, mut unmatched) = (0, 0, 0);

    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        let mut ops = deserialize_golden(&file, &text);
        // Goldens stamp placeholder provenance ids; cursor rows are the
        // pipeline's job, so strip them here rather than fake a cursor table.
        for op in &mut ops {
            if let IngestOp::Message(row) = op {
                row.provenance.src_cursor = None;
                row.provenance.src_offset = None;
            }
        }
        store
            .apply(&Batch {
                ops: scaffolding(&ops, &runtime_id, kind),
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        let stats = store
            .apply(&Batch {
                ops,
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        inserted += stats.messages_inserted;
        merged += stats.tool_results_merged;
        unmatched += stats.tool_results_unmatched;
    }

    // The projection must be searchable afterwards — FTS stayed in sync.
    let hits = queries::search(store.conn(), "rosetta OR mock OR synthetic", 10).unwrap();
    assert!(
        !hits.is_empty(),
        "{adapter}: FTS found nothing across all goldens"
    );
    (inserted, merged, unmatched)
}

#[test]
fn claude_code_goldens_apply_cleanly() {
    let (inserted, merged, unmatched) = apply_adapter_goldens("claude-code", "claude-code");
    assert!(inserted > 0);
    assert!(
        merged > 0,
        "tool-use fixtures must exercise the result merge"
    );
    assert_eq!(unmatched, 0, "every tool result must find its call row");
}

#[test]
fn codex_goldens_apply_cleanly() {
    let (inserted, merged, unmatched) = apply_adapter_goldens("codex", "codex");
    assert!(inserted > 0);
    assert!(
        merged > 0,
        "tool-use fixtures must exercise the result merge"
    );
    assert_eq!(unmatched, 0, "every tool result must find its call row");
}
