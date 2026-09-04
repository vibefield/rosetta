//! On-demand recovery of one command from the append-only raw ledger.
//!
//! The disposable projection keeps one 42-character display preview. Expansion replays the
//! command's one source generation through the installed adapter, so native
//! JSON shapes still have exactly one parser and never leak into the server
//! or renderer.

use std::collections::BTreeSet;
use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, NormalizeCtx};
use rosetta_capture::{CaptureError, LedgerEntry};
use rosetta_core::{CanonicalText, IngestOp, TimelineBody, ToolAction};

use crate::PipelineError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommandLocator {
    pub runtime_id: String,
    pub session_id: String,
    pub cursor_id: i64,
    pub relative_path: String,
    pub generation: u64,
    pub offset: u64,
    pub timeline_id: String,
    pub correlation_key: String,
}

#[derive(Debug)]
enum VisitError {
    Capture(CaptureError),
    Found,
}

impl From<CaptureError> for VisitError {
    fn from(error: CaptureError) -> Self {
        Self::Capture(error)
    }
}

fn text_from_action(action: &ToolAction) -> Option<&CanonicalText> {
    match action {
        ToolAction::Shell { command, .. } => Some(command),
        ToolAction::CodeExecute { code, .. } => Some(code),
        _ => None,
    }
}

fn command_from_ops(ops: &[IngestOp], timeline_id: &str, correlation_key: &str) -> Option<String> {
    let text = ops.iter().find_map(|op| {
        let IngestOp::Message(entry) = op else {
            return None;
        };
        let TimelineBody::ToolUse { tool } = &entry.body else {
            return None;
        };
        if entry.timeline_id != timeline_id && tool.correlation.key != correlation_key {
            return None;
        }
        text_from_action(&tool.action).cloned()
    })?;
    match text {
        CanonicalText::Inline { text } => Some(text),
        CanonicalText::Member { asset, .. } => ops.iter().find_map(|op| {
            let IngestOp::ProjectionAsset { asset: member } = op else {
                return None;
            };
            if member.digest != asset.digest {
                return None;
            }
            String::from_utf8(member.bytes.clone()).ok()
        }),
    }
}

/// Read and normalize only the source generation that owns `locator`. The
/// scan stops at the target record; previous records from that source are fed
/// through only to restore adapter-local correlation state.
pub fn read_raw_command(
    adapter: &dyn AgentAdapter,
    ledger_dir: &Path,
    locator: &RawCommandLocator,
) -> Result<Option<String>, PipelineError> {
    let mut context = NormalizeCtx::new(&locator.runtime_id, &locator.session_id);
    context.state = adapter.restore_state(None).map_err(PipelineError::Config)?;
    context.src_cursor = Some(locator.cursor_id);
    context.source_identity = Some(locator.relative_path.clone());
    context.source_generation = locator.generation;
    let mut source_initialized = false;
    let mut seen = BTreeSet::new();
    let mut found = None;
    // The segment index narrows the walk to the segments holding this source
    // generation; unindexed segments (the open one, or all of them for a
    // ledger without an index) are always included.
    let candidates = rosetta_capture::candidate_segments(
        ledger_dir,
        &locator.session_id,
        &locator.relative_path,
        locator.generation,
    )
    .map_err(PipelineError::Capture)?;
    let visited = rosetta_capture::visit_ledger_segments::<VisitError, _>(
        ledger_dir,
        |seq| candidates.contains(&seq),
        |entry| {
            let LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } = entry
            else {
                return Ok(());
            };
            if source.relative_path != locator.relative_path
                || source.session_id != locator.session_id
                || generation != locator.generation
                || !seen.insert(start_offset)
            {
                return Ok(());
            }

            if !source_initialized {
                context.native_session_id = Some(source.native_session_id.clone());
                context.agent_version = source.agent_version.clone();
                source_initialized = true;
            }
            context.agent_version = source.agent_version;
            context.src_offset = i64::try_from(start_offset).ok();
            let outcome = adapter.ingest_line(&mut context, &source.stream_id, &payload);
            if start_offset != locator.offset {
                return Ok(());
            }
            found = command_from_ops(&outcome.ops, &locator.timeline_id, &locator.correlation_key);
            Err(VisitError::Found)
        },
    );

    match visited {
        Ok(_) | Err(VisitError::Found) => Ok(found),
        Err(VisitError::Capture(error)) => Err(PipelineError::Capture(error)),
    }
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    use rosetta_adapter_support::{AgentAdapter, NormalizeCtx};
    use rosetta_capture::{Ledger, SourceDef};
    use rosetta_core::{IngestOp, TimelineBody, ToolAction};
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn only_command_families_expose_source_text() {
        assert!(super::text_from_action(&ToolAction::Unknown {}).is_none());
    }

    #[test]
    fn exact_command_is_recovered_by_reusing_the_adapter_on_raw_bytes() {
        let full = "one\ntwo\nthree\nfour\nfive\nsix\nseven";
        let line = json!({
            "type": "assistant",
            "parentUuid": null,
            "isSidechain": false,
            "message": {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-5",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "Bash",
                    "input": {"command": full}
                }],
                "stop_reason": "tool_use",
                "stop_sequence": null
            },
            "uuid": "11111111-1111-1111-1111-111111111111",
            "timestamp": "2026-08-25T02:35:19.072Z",
            "sessionId": "native-session",
            "version": "2.1.241",
            "cwd": "/tmp/project",
            "gitBranch": "main",
            "userType": "external"
        })
        .to_string();

        let adapter = ClaudeCodeAdapter;
        let mut context = NormalizeCtx::new("rt1", "s1");
        context.native_session_id = Some("native-session".into());
        context.state = adapter.init_state();
        context.src_cursor = Some(7);
        context.src_offset = Some(0);
        context.source_identity = Some("projects/session.jsonl".into());
        context.source_generation = 1;
        let normalized = adapter.ingest_line(&mut context, "transcript", line.as_bytes());
        let (timeline_id, correlation_key) = normalized
            .ops
            .iter()
            .find_map(|op| {
                let IngestOp::Message(entry) = op else {
                    return None;
                };
                let TimelineBody::ToolUse { tool } = &entry.body else {
                    return None;
                };
                Some((entry.timeline_id.clone(), tool.correlation.key.clone()))
            })
            .expect("Claude command row");

        let directory = TempDir::new().unwrap();
        let source = SourceDef {
            relative_path: "projects/session.jsonl".into(),
            stream_id: "transcript".into(),
            session_id: "s1".into(),
            native_session_id: "native-session".into(),
            agent_version: Some("2.1.241".into()),
        };
        let mut ledger = Ledger::open(directory.path()).unwrap();
        ledger
            .append_raw(&source, 1, [(0, line.as_bytes())])
            .unwrap();
        ledger.flush().unwrap();

        let recovered = super::read_raw_command(
            &adapter,
            directory.path(),
            &super::RawCommandLocator {
                runtime_id: "rt1".into(),
                session_id: "s1".into(),
                cursor_id: 7,
                relative_path: source.relative_path,
                generation: 1,
                offset: 0,
                timeline_id,
                correlation_key,
            },
        )
        .unwrap();
        assert_eq!(recovered.as_deref(), Some(full));
    }

    #[test]
    fn the_segment_index_narrows_the_walk_to_the_owning_segment() {
        let line = json!({
            "type": "assistant",
            "parentUuid": null,
            "isSidechain": false,
            "message": {
                "id": "msg_9",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-5",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_9",
                    "name": "Bash",
                    "input": {"command": "cargo test --workspace"}
                }],
                "stop_reason": "tool_use",
                "stop_sequence": null
            },
            "uuid": "99999999-9999-9999-9999-999999999999",
            "timestamp": "2026-08-25T02:35:19.072Z",
            "sessionId": "native-session",
            "version": "2.1.241",
            "cwd": "/tmp/project",
            "gitBranch": "main",
            "userType": "external"
        })
        .to_string();
        let adapter = ClaudeCodeAdapter;
        let mut context = NormalizeCtx::new("rt1", "s1");
        context.native_session_id = Some("native-session".into());
        context.state = adapter.init_state();
        context.src_cursor = Some(9);
        context.src_offset = Some(0);
        context.source_identity = Some("projects/session.jsonl".into());
        context.source_generation = 4;
        let normalized = adapter.ingest_line(&mut context, "transcript", line.as_bytes());
        let (timeline_id, correlation_key) = normalized
            .ops
            .iter()
            .find_map(|op| {
                let IngestOp::Message(entry) = op else {
                    return None;
                };
                let TimelineBody::ToolUse { tool } = &entry.body else {
                    return None;
                };
                Some((entry.timeline_id.clone(), tool.correlation.key.clone()))
            })
            .expect("Claude command row");

        let directory = TempDir::new().unwrap();
        let other = SourceDef {
            relative_path: "projects/other.jsonl".into(),
            stream_id: "transcript".into(),
            session_id: "s2".into(),
            native_session_id: "native-other".into(),
            agent_version: Some("2.1.241".into()),
        };
        let source = SourceDef {
            relative_path: "projects/session.jsonl".into(),
            stream_id: "transcript".into(),
            session_id: "s1".into(),
            native_session_id: "native-session".into(),
            agent_version: Some("2.1.241".into()),
        };
        // A one-byte cap rotates at every flush: four segments of noise from
        // another session and earlier generations, then the owning one.
        let mut ledger = Ledger::open(directory.path())
            .unwrap()
            .with_max_segment_bytes(1);
        for generation in 1..=2 {
            ledger
                .append_raw(&other, generation, [(0, b"not even json".as_slice())])
                .unwrap();
            ledger.flush().unwrap();
            ledger
                .append_raw(
                    &source,
                    generation,
                    [(0, b"{\"type\":\"progress\"}".as_slice())],
                )
                .unwrap();
            ledger.flush().unwrap();
        }
        ledger
            .append_raw(&source, 4, [(0, line.as_bytes())])
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        let candidates = rosetta_capture::candidate_segments(
            directory.path(),
            "s1",
            "projects/session.jsonl",
            4,
        )
        .unwrap();
        assert_eq!(candidates.into_iter().collect::<Vec<_>>(), [4]);

        let recovered = super::read_raw_command(
            &adapter,
            directory.path(),
            &super::RawCommandLocator {
                runtime_id: "rt1".into(),
                session_id: "s1".into(),
                cursor_id: 9,
                relative_path: source.relative_path,
                generation: 4,
                offset: 0,
                timeline_id,
                correlation_key,
            },
        )
        .unwrap();
        assert_eq!(recovered.as_deref(), Some("cargo test --workspace"));
    }
}
