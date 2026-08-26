//! Checkpoint persistence and split/resume normalization equivalence.

use std::path::{Path, PathBuf};

use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_support::adapter::{AgentAdapter, NormalizeCtx};
use rosetta_core::fold::CumulativeUsage;
use rosetta_core::{IngestOp, ids};

fn tool_fixture() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../groundtruth/copilot/fixtures/1.0.80/tool-use/copilot-home/session-state");
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| entry.file_name() == "events.jsonl")
        .expect("tool-use events fixture")
        .into_path()
}

fn context() -> NormalizeCtx {
    let runtime_id = ids::runtime_id("copilot", "~/.copilot");
    let session_id = ids::session_id(&runtime_id, "5857326d-2a15-4f6c-9eac-1df1b2989290");
    let mut ctx = NormalizeCtx::new(runtime_id, session_id);
    // The pipeline carries skim's native version into a resumed context.
    ctx.agent_version = Some("1.0.80".to_owned());
    ctx.state = CopilotAdapter.init_state();
    ctx
}

fn ingest(lines: &[&str], ctx: &mut NormalizeCtx, cursor_base: usize) -> Vec<IngestOp> {
    let mut ops = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        ctx.src_cursor = Some((cursor_base + index + 1) as i64);
        let outcome = CopilotAdapter.ingest_line(ctx, "events", line.as_bytes());
        assert_eq!(
            outcome.status,
            rosetta_adapter_support::adapter::LineStatus::Parsed
        );
        ops.extend(outcome.ops);
    }
    ops
}

#[test]
fn checkpoint_serde_round_trip_and_bad_payload_errors() {
    let text = std::fs::read_to_string(tool_fixture()).unwrap();
    let lines: Vec<_> = text.lines().collect();
    let split = lines
        .iter()
        .position(|line| line.contains("tool.execution_start"))
        .expect("tool start")
        + 1;
    let mut ctx = context();
    ingest(&lines[..split], &mut ctx, 0);

    let checkpoint = CopilotAdapter
        .checkpoint_state(ctx.state.as_deref())
        .expect("serialize state")
        .expect("state payload");
    let restored = CopilotAdapter
        .restore_state(Some(&checkpoint))
        .expect("restore state")
        .expect("restored state");
    let rendered_again = CopilotAdapter
        .checkpoint_state(Some(restored.as_ref()))
        .expect("serialize restored state")
        .expect("state payload");
    assert_eq!(checkpoint, rendered_again);

    assert!(
        CopilotAdapter
            .restore_state(Some(&serde_json::json!({"seen_tools": "wrong"})))
            .is_err(),
        "bad checkpoints must expire rather than silently reset"
    );
}

#[test]
fn split_resume_matches_one_shot_ops() {
    let text = std::fs::read_to_string(tool_fixture()).unwrap();
    let lines: Vec<_> = text.lines().collect();
    let split = lines
        .iter()
        .position(|line| line.contains("tool.execution_start"))
        .expect("tool start")
        + 1;

    let mut one_shot_ctx = context();
    let one_shot = ingest(&lines, &mut one_shot_ctx, 0);

    let mut first_ctx = context();
    let mut resumed_ops = ingest(&lines[..split], &mut first_ctx, 0);
    let checkpoint = CopilotAdapter
        .checkpoint_state(first_ctx.state.as_deref())
        .unwrap();
    let next_ordinal = first_ctx.next_ordinal;
    let cumulative = *first_ctx.cumulative.last_reading();

    let mut resumed_ctx = context();
    resumed_ctx.state = CopilotAdapter
        .restore_state(checkpoint.as_ref())
        .expect("restore checkpoint");
    resumed_ctx.next_ordinal = next_ordinal;
    resumed_ctx.cumulative = CumulativeUsage::resume(cumulative);
    resumed_ops.extend(ingest(&lines[split..], &mut resumed_ctx, split));

    assert_eq!(resumed_ops, one_shot);
}
