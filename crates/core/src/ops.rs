//! The central op type: adapters end here, the writer starts here. All
//! agent-specific weirdness stops at this boundary (docs/ingest-engine.html).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::assets::ProjectionAsset;
use crate::rows::{
    DriftFact, FileEditRow, GitCommitRow, MessageRow, Ms, PlanRow, ProjectRow, RuntimeRow,
    SessionRow, SessionTitleRow, SessionUsageRow, TaskRow, TitleKind, TokenSnapshot,
};
use crate::timeline::{
    EvidenceOrder, ExecutionMode, GroupDerivation, GroupKind, Notice, PermissionOutcome,
    Provenance, RuntimeErrorKind, SummaryPurpose, TimelineBody, TimelineEntry, TimelineGroup,
    ToolUpdatePatch, TruncationScope, WorkflowJournalObservation, WorkflowRun,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IngestOp {
    /// A decoded media attachment or oversized canonical text member. The
    /// bytes are materialized outside SQLite before a typed reference to its
    /// stable `/api/assets/<digest>.<ext>` URL can commit.
    ProjectionAsset {
        asset: ProjectionAsset,
    },
    Runtime(RuntimeRow),
    Project(ProjectRow),
    /// Meta-phase upsert. Stream normalization never mutates session identity,
    /// topology, cwd, or project ownership.
    SessionMeta(SessionRow),
    /// Ranked title fact recovered during bounded metadata admission.
    SessionTitle(SessionTitleRow),
    /// One closed canonical timeline entry (upsert by timeline_id). Boxed: it
    /// is by far the largest variant and ops move through channels in bulk.
    Message(Box<TimelineEntry>),
    /// A typed merge op, not a row: folds into its tool_use row by the
    /// adapter's deterministic correlation key.  Native result bodies never
    /// cross this boundary.
    ToolUpdate {
        session_id: String,
        correlation_key: String,
        evidence: EvidenceOrder,
        patch: Box<ToolUpdatePatch>,
        at: Option<Ms>,
    },
    /// Replacement-keyed usage snapshot for a response row that already exists
    /// (e.g. codex token_count events correcting the prior assistant row).
    /// The writer applies (new − old) to the hour bucket; absent components
    /// stay `None` — never coerced to zero.
    Usage {
        session_id: String,
        /// timeline_id of the response row this snapshot belongs to.
        response: String,
        ts: Ms,
        model: Option<String>,
        tokens: TokenSnapshot,
        /// Stable native identity of the model response this usage belongs
        /// to, shared by every copy of the response across files (Claude
        /// Code: `<message.id>:<requestId>`). The store keeps the usage of
        /// the earliest-started session's copy and drops the others'
        /// (ARCHITECTURE §4.1.3, counting rule 2). Never persisted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<String>,
    },
    /// Usage that the native agent reports only at session/task granularity.
    /// It is an additive canonical sample, not a synthetic response message.
    SessionUsage(SessionUsageRow),
    /// Cumulative session-tree totals, reconciled against recorded requests.
    SessionUsageTotals(crate::SessionUsageTotals),
    FileEdit(FileEditRow),
    Plan(PlanRow),
    Task(TaskRow),
    /// Current durable snapshot for one Workflow run. Attempts remain tool
    /// timeline rows and join this document by `run_id` at read time.
    WorkflowRun {
        session_id: String,
        run: Box<WorkflowRun>,
    },
    /// One append-only coordinator observation for a Workflow worker.
    WorkflowJournal {
        session_id: String,
        observation: WorkflowJournalObservation,
    },
    GitCommit(GitCommitRow),
    /// Lands aggregated in drift_agg — never one row per bad record.
    Drift(DriftFact),
}

impl IngestOp {
    /// Close a normalizer row into the public timeline. Returns whether it
    /// survived as a timeline message; a row that did not cannot be the
    /// target of a later `Usage` op (its own usage became a session sample).
    pub fn push_message(ops: &mut Vec<Self>, row: MessageRow) -> bool {
        let projected = Self::project_message(row);
        let survived = projected.iter().any(|op| matches!(op, Self::Message(_)));
        ops.extend(projected);
        survived
    }

    /// Close a tool update into the same bounded projection used by opener
    /// messages. Result records can carry better request evidence than the
    /// opener (for example Claude Code's contextual `structuredPatch`), and
    /// those diffs need the same content-addressed overflow path as an
    /// opener's action before SQLite validates the merged tool.
    pub fn push_tool_update(
        ops: &mut Vec<Self>,
        session_id: String,
        correlation_key: String,
        evidence: EvidenceOrder,
        mut patch: ToolUpdatePatch,
        at: Option<Ms>,
    ) {
        let mut assets = Vec::new();
        for action in [
            patch.request.as_mut().map(|request| &mut request.action),
            patch.outcome.as_mut().map(|outcome| &mut outcome.action),
        ]
        .into_iter()
        .flatten()
        {
            assets.extend(action.externalize_text_members());
        }
        ops.extend(
            assets
                .into_iter()
                .map(|asset| Self::ProjectionAsset { asset }),
        );
        ops.push(Self::ToolUpdate {
            session_id,
            correlation_key,
            evidence,
            patch: Box::new(patch),
            at,
        });
    }

    /// Close a Workflow sidecar into the same bounded member store as message
    /// text. The run remains one row even when its script or result needs a
    /// content-addressed overflow member.
    pub fn push_workflow_run(ops: &mut Vec<Self>, session_id: String, mut run: WorkflowRun) {
        ops.extend(
            run.externalize_text_members()
                .into_iter()
                .map(|asset| Self::ProjectionAsset { asset }),
        );
        ops.push(Self::WorkflowRun {
            session_id,
            run: Box::new(run),
        });
    }

    /// Close command/code payloads into their bounded SQLite projection.
    /// Adapter normalization remains lossless until this explicit boundary,
    /// so an on-demand raw-ledger read can run the same parser and recover the
    /// exact source text without a second native-format implementation.
    pub fn compact_command_previews(ops: &mut Vec<Self>) {
        let mut retired_members = BTreeSet::new();
        for op in ops.iter_mut() {
            match op {
                Self::Message(entry) => {
                    if let TimelineBody::ToolUse { tool } = &mut entry.body
                        && let Some(digest) = tool.action.compact_command_preview()
                    {
                        retired_members.insert(digest);
                    }
                }
                Self::ToolUpdate { patch, .. } => {
                    for action in [
                        patch.request.as_mut().map(|request| &mut request.action),
                        patch.outcome.as_mut().map(|outcome| &mut outcome.action),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if let Some(digest) = action.compact_command_preview() {
                            retired_members.insert(digest);
                        }
                    }
                }
                _ => {}
            }
        }
        if retired_members.is_empty() {
            return;
        }

        // A hash may legitimately be shared with another canonical text
        // member in the same batch. Keep that asset whenever any remaining
        // typed op still names it; otherwise the raw ledger is its sole home.
        let mut still_referenced = BTreeSet::new();
        for op in ops.iter() {
            if matches!(op, Self::ProjectionAsset { .. }) {
                continue;
            }
            let Ok(encoded) = serde_json::to_string(op) else {
                continue;
            };
            for digest in &retired_members {
                if encoded.contains(digest) {
                    still_referenced.insert(digest.clone());
                }
            }
        }
        ops.retain(|op| match op {
            Self::ProjectionAsset { asset } => {
                !retired_members.contains(&asset.digest) || still_referenced.contains(&asset.digest)
            }
            _ => true,
        });
    }

    /// Close a normalizer-local message draft into the public timeline
    /// contract. Oversized prose becomes a content-addressed member emitted
    /// before its row; ledger-only system/context records produce no message.
    /// Usage on an otherwise invisible response becomes a session sample so
    /// narrowing can never erase accounting facts.
    pub fn project_message(mut row: MessageRow) -> Vec<Self> {
        let fallback_usage = usage_only(&row);
        if matches!(
            row.subtype.as_deref(),
            Some("ai_title" | "custom_title" | "title_fallback")
        ) {
            let Some(title) = nonempty(row.content) else {
                return fallback_usage.into_iter().collect();
            };
            let kind = match row.subtype.as_deref() {
                Some("ai_title") => TitleKind::Generated,
                Some("custom_title") => TitleKind::Explicit,
                _ => TitleKind::Fallback,
            };
            return vec![Self::SessionTitle(SessionTitleRow {
                session_id: row.session_id,
                title,
                kind,
                at: row.ts.unwrap_or(0),
            })];
        }

        let mut assets = Vec::new();
        // Range offsets describe Unicode scalars in the complete authored text,
        // including when that text is externalized below. A bad annotation must
        // never point into a preview or erase the owning message/image.
        if row
            .attachments
            .iter()
            .any(|attachment| !attachment.message_ranges.is_empty())
        {
            let len = row
                .content
                .as_deref()
                .map_or(0, |text| text.chars().count());
            for attachment in &mut row.attachments {
                attachment
                    .message_ranges
                    .retain(|range| range.start < range.end && range.end as usize <= len);
            }
        }
        let content = nonempty(row.content).map(|content| {
            let (text, asset) = crate::CanonicalText::externalize(content);
            if let Some(asset) = asset {
                assets.push(Self::ProjectionAsset { asset });
            }
            text
        });
        let body = match row.kind {
            crate::rows::MessageKind::User if content.is_some() || !row.attachments.is_empty() => {
                TimelineBody::User {
                    text: content,
                    attachments: row.attachments,
                }
            }
            crate::rows::MessageKind::Agent
                if content.is_some()
                    || !row.attachments.is_empty()
                    || !row.citations.is_empty() =>
            {
                TimelineBody::Agent {
                    text: content,
                    attachments: row.attachments,
                    citations: row.citations,
                }
            }
            crate::rows::MessageKind::Thinking => {
                let Some(text) = content else {
                    return fallback_usage.into_iter().collect();
                };
                TimelineBody::Thinking { text }
            }
            crate::rows::MessageKind::ToolUse => {
                let Some(mut tool) = row.tool else {
                    return fallback_usage.into_iter().collect();
                };
                assets.extend(
                    tool.action
                        .externalize_text_members()
                        .into_iter()
                        .map(|asset| Self::ProjectionAsset { asset }),
                );
                TimelineBody::ToolUse {
                    tool: Box::new(tool),
                }
            }
            crate::rows::MessageKind::System => {
                let Some(notice) = legacy_notice(row.subtype.as_deref(), content.as_ref()) else {
                    return fallback_usage.into_iter().collect();
                };
                TimelineBody::Notice { notice }
            }
            crate::rows::MessageKind::Summary => {
                let Some(text) = content else {
                    return fallback_usage.into_iter().collect();
                };
                TimelineBody::Summary {
                    purpose: summary_purpose(row.subtype.as_deref()),
                    text,
                }
            }
            crate::rows::MessageKind::User | crate::rows::MessageKind::Agent => {
                return fallback_usage.into_iter().collect();
            }
        };

        let response = row.response.filter(|_| row.block_index.is_some());
        let block_index = row.block_index.filter(|_| response.is_some());
        let entry = TimelineEntry {
            timeline_id: row.timeline_id.clone(),
            session_id: row.session_id.clone(),
            ordinal: row.ordinal,
            group: row.group.unwrap_or(TimelineGroup {
                id: row.timeline_id,
                kind: GroupKind::Synthetic,
                derivation: GroupDerivation::Synthetic,
            }),
            response,
            block_index,
            native_id: row.uuid,
            parent_native_id: row.parent_uuid,
            at: row.ts,
            model: row.model,
            effort: row.effort,
            body,
            parent_tool_native_id: row.branch_tool_id,
            usage: row.usage.unwrap_or_default(),
            cost: None,
            provenance: Provenance {
                agent_version: row.agent_version,
                src_cursor: row.src_cursor,
                src_offset: row.src_offset,
            },
        };
        if entry.validate().is_err() {
            return fallback_usage.into_iter().collect();
        }
        assets.push(Self::Message(Box::new(entry)));
        assets
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn usage_only(row: &MessageRow) -> Option<IngestOp> {
    let usage = row.usage.clone().filter(|usage| !usage.is_empty())?;
    Some(IngestOp::SessionUsage(SessionUsageRow {
        identity: None,
        session_id: row.session_id.clone(),
        key: format!("timeline:{}", row.timeline_id),
        ts: row.ts.unwrap_or(0),
        model: row.model.clone(),
        tokens: usage,
    }))
}

fn summary_purpose(subtype: Option<&str>) -> SummaryPurpose {
    let subtype = subtype.unwrap_or_default().to_ascii_lowercase();
    if subtype.contains("branch") {
        SummaryPurpose::Branch
    } else if subtype.contains("handoff") {
        SummaryPurpose::Handoff
    } else if subtype.contains("compact") || subtype.contains("checkpoint") {
        SummaryPurpose::Compaction
    } else {
        SummaryPurpose::Session
    }
}

/// Conservative compatibility classification while adapters move each native
/// lifecycle record to an explicit notice constructor. Unrecognized system
/// strings—including injected prompts and environment context—stay ledger-only.
fn legacy_notice(subtype: Option<&str>, content: Option<&crate::CanonicalText>) -> Option<Notice> {
    let subtype = subtype?.to_ascii_lowercase();
    if subtype == "command_output" {
        return content
            .cloned()
            .map(|output| Notice::CommandOutput { output });
    }
    if subtype.contains("compact") {
        return Some(Notice::Compaction { reason: None });
    }
    if subtype.contains("truncat") || subtype.contains("context_limit") {
        return Some(Notice::Truncation {
            scope: TruncationScope::Session,
            omitted_count: None,
        });
    }
    if subtype.contains("permission") && subtype.contains("request") {
        return Some(Notice::PermissionRequested {
            request_id: None,
            scope: None,
        });
    }
    if subtype.contains("permission") && subtype.contains("den") {
        return Some(Notice::PermissionDecision {
            decision: PermissionOutcome::Denied,
            scope: None,
        });
    }
    if subtype.contains("permission") && subtype.contains("cancel") {
        return Some(Notice::PermissionDecision {
            decision: PermissionOutcome::Cancelled,
            scope: None,
        });
    }
    if subtype.contains("handoff") {
        return Some(Notice::Handoff {
            to_session_id: None,
            to_agent: None,
        });
    }
    if subtype.contains("mode") {
        let mode = if subtype.contains("plan") {
            ExecutionMode::Plan
        } else if subtype.contains("review") {
            ExecutionMode::Review
        } else if subtype.contains("worktree") {
            ExecutionMode::Worktree
        } else {
            ExecutionMode::Normal
        };
        return Some(Notice::ModeChanged {
            from: None,
            to: mode,
        });
    }
    if subtype.contains("error") || subtype.contains("fail") || subtype.contains("retry") {
        let summary = content
            .map(crate::CanonicalText::as_preview)
            .map(|value| utf8_prefix(value, crate::timeline::FAILURE_SUMMARY_MAX_BYTES).to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| subtype.clone());
        return Some(Notice::RuntimeError {
            kind: RuntimeErrorKind::Unknown,
            summary,
        });
    }
    None
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use crate::IngestOp;
    use crate::rows::{MessageKind, MessageRow, TokenSnapshot};
    use crate::timeline::{
        CanonicalDocument, CanonicalText, CorrelationDerivation, DisplayField, DisplayValue,
        DocumentBlock, FieldRole, INLINE_TEXT_MAX_BYTES, TimelineBody, ToolAction, ToolInteraction,
    };

    fn message(kind: MessageKind, content: Option<String>) -> MessageRow {
        MessageRow {
            timeline_id: "m1".into(),
            session_id: "s1".into(),
            ordinal: 0,
            kind,
            subtype: None,
            uuid: Some("native-m1".into()),
            parent_uuid: None,
            ts: Some(1_000),
            model: Some("model".into()),
            effort: None,
            content,
            tool: None,
            attachments: Vec::new(),
            citations: Vec::new(),
            group: None,
            response: None,
            block_index: None,
            branch_tool_id: None,
            usage: None,
            agent_version: Some("1.0".into()),
            src_cursor: Some(1),
            src_offset: Some(2),
        }
    }

    #[test]
    fn oversized_prose_becomes_one_member_before_its_entry() {
        let prose = "界".repeat(INLINE_TEXT_MAX_BYTES);
        let ops = IngestOp::project_message(message(MessageKind::Agent, Some(prose.clone())));
        let [
            IngestOp::ProjectionAsset { asset },
            IngestOp::Message(entry),
        ] = ops.as_slice()
        else {
            panic!("expected one member followed by one message, got {ops:#?}");
        };
        assert_eq!(asset.bytes, prose.as_bytes());
        assert_eq!(asset.media_type, "text/plain; charset=utf-8");
        let TimelineBody::Agent {
            text:
                Some(CanonicalText::Member {
                    asset: reference,
                    preview,
                    byte_len,
                }),
            ..
        } = &entry.body
        else {
            panic!("expected member-backed agent prose, got {:?}", entry.body);
        };
        assert!(preview.len() <= crate::timeline::TEXT_PREVIEW_MAX_BYTES);
        assert!(preview.is_char_boundary(preview.len()));
        assert_eq!(*byte_len, prose.len() as u64);
        assert_eq!(reference.digest, asset.digest);
        assert!(entry.validate().is_ok());
    }

    #[test]
    fn oversized_tool_intent_becomes_one_member_before_its_entry() {
        let command = "echo important-user-intent ".repeat(INLINE_TEXT_MAX_BYTES / 4);
        let mut row = message(MessageKind::ToolUse, None);
        row.tool = Some(ToolInteraction {
            attachments: Vec::new(),
            action: ToolAction::Shell {
                // Adapter projectors intentionally produce this typed draft;
                // the central boundary owns member creation.
                command: CanonicalText::Inline {
                    text: command.clone(),
                },
                source_truncated: false,
                cwd: None,
                timeout_ms: None,
                background: false,
                outcome: None,
            },
            ..ToolInteraction::unknown("tool-1", CorrelationDerivation::NativeId, "Bash")
        });

        let ops = IngestOp::project_message(row);
        let [
            IngestOp::ProjectionAsset { asset },
            IngestOp::Message(entry),
        ] = ops.as_slice()
        else {
            panic!("expected one member followed by one message, got {ops:#?}");
        };
        assert_eq!(asset.bytes, command.as_bytes());
        let TimelineBody::ToolUse { tool } = &entry.body else {
            panic!("expected tool row");
        };
        let ToolAction::Shell { command, .. } = &tool.action else {
            panic!("expected shell action");
        };
        assert!(matches!(command, CanonicalText::Member { .. }));
        assert!(entry.validate().is_ok());
    }

    #[test]
    fn command_projection_keeps_one_line_and_retires_the_full_member() {
        let command = (1..=7)
            .map(|line| format!("printf line-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut row = message(MessageKind::ToolUse, None);
        row.tool = Some(ToolInteraction {
            attachments: Vec::new(),
            action: ToolAction::Shell {
                command: CanonicalText::Inline {
                    text: command.clone(),
                },
                source_truncated: false,
                cwd: None,
                timeout_ms: None,
                background: false,
                outcome: None,
            },
            ..ToolInteraction::unknown("tool-1", CorrelationDerivation::NativeId, "Bash")
        });

        let mut ops = IngestOp::project_message(row);
        IngestOp::compact_command_previews(&mut ops);
        let [IngestOp::Message(entry)] = ops.as_slice() else {
            panic!("command previews retain only their message, got {ops:#?}");
        };
        let TimelineBody::ToolUse { tool } = &entry.body else {
            panic!("expected tool row");
        };
        let ToolAction::Shell {
            command,
            source_truncated,
            ..
        } = &tool.action
        else {
            panic!("expected shell action");
        };
        assert_eq!(command.as_preview(), "printf line-1");
        assert!(*source_truncated);

        // Store application is a second defensive projection boundary. It
        // must not mistake an already-bounded preview for complete source.
        let mut action = tool.action.clone();
        assert_eq!(action.compact_command_preview(), None);
        let ToolAction::Shell {
            source_truncated, ..
        } = action
        else {
            unreachable!()
        };
        assert!(source_truncated);

        // Oversized commands used to leave a complete projection member on
        // disk. Compaction converts it to the same source-backed preview and
        // removes that redundant asset from the batch.
        let huge = (0..8)
            .map(|line| format!("{line}:{}", "x".repeat(INLINE_TEXT_MAX_BYTES)))
            .collect::<Vec<_>>()
            .join("\n");
        let mut huge_row = message(MessageKind::ToolUse, None);
        huge_row.tool = Some(ToolInteraction {
            attachments: Vec::new(),
            action: ToolAction::Shell {
                command: CanonicalText::Inline { text: huge },
                source_truncated: false,
                cwd: None,
                timeout_ms: None,
                background: false,
                outcome: None,
            },
            ..ToolInteraction::unknown("tool-2", CorrelationDerivation::NativeId, "Bash")
        });
        let mut huge_ops = IngestOp::project_message(huge_row);
        assert!(matches!(huge_ops[0], IngestOp::ProjectionAsset { .. }));
        IngestOp::compact_command_previews(&mut huge_ops);
        assert!(matches!(huge_ops.as_slice(), [IngestOp::Message(_)]));
    }

    #[test]
    fn complete_42_character_command_is_not_marked_truncated() {
        let complete = "x".repeat(crate::timeline::COMMAND_PREVIEW_MAX_CHARACTERS);
        let mut action = ToolAction::Shell {
            command: CanonicalText::inline(&complete).unwrap(),
            source_truncated: false,
            cwd: None,
            timeout_ms: None,
            background: false,
            outcome: None,
        };
        assert_eq!(action.compact_command_preview(), None);
        let ToolAction::Shell {
            command,
            source_truncated,
            ..
        } = action
        else {
            unreachable!()
        };
        assert_eq!(command.as_preview(), complete);
        assert!(!source_truncated);
    }

    #[test]
    fn omitted_unicode_command_reserves_room_for_the_ui_marker() {
        let source = format!("{}終", "é".repeat(42));
        let mut action = ToolAction::Shell {
            command: CanonicalText::inline(source).unwrap(),
            source_truncated: false,
            cwd: None,
            timeout_ms: None,
            background: false,
            outcome: None,
        };
        assert_eq!(action.compact_command_preview(), None);
        let ToolAction::Shell {
            command,
            source_truncated,
            ..
        } = action
        else {
            unreachable!()
        };
        assert_eq!(command.as_preview(), "é".repeat(39));
        assert!(source_truncated);
    }

    #[test]
    fn oversized_structured_document_becomes_one_typed_member() {
        let blocks = (0..40)
            .map(|index| DocumentBlock::Field {
                field: DisplayField {
                    role: FieldRole::Outcome,
                    label: format!("field {index}"),
                    value: DisplayValue::Text("x".repeat(1_000)),
                },
            })
            .collect();
        let mut row = message(MessageKind::ToolUse, None);
        row.tool = Some(ToolInteraction {
            attachments: Vec::new(),
            action: ToolAction::StructuredOutput {
                document: CanonicalDocument::Inline {
                    title: Some("Report".into()),
                    summary: None,
                    blocks,
                    resources: Vec::new(),
                },
            },
            ..ToolInteraction::unknown(
                "tool-structured",
                CorrelationDerivation::NativeId,
                "StructuredOutput",
            )
        });

        let ops = IngestOp::project_message(row);
        let [
            IngestOp::ProjectionAsset { asset },
            IngestOp::Message(entry),
        ] = ops.as_slice()
        else {
            panic!("expected one document member and one row, got {ops:#?}");
        };
        assert_eq!(
            asset.media_type,
            "application/vnd.rosetta.canonical-document+json"
        );
        assert!(serde_json::from_slice::<serde_json::Value>(&asset.bytes).is_ok());
        let TimelineBody::ToolUse { tool } = &entry.body else {
            panic!("expected tool row");
        };
        let ToolAction::StructuredOutput {
            document: CanonicalDocument::Member {
                asset: reference, ..
            },
        } = &tool.action
        else {
            panic!("expected member-backed structured output");
        };
        assert_eq!(reference.digest, asset.digest);
        assert!(entry.validate().is_ok());
    }

    #[test]
    fn injected_context_stays_ledger_only_but_usage_is_not_erased() {
        let mut context = message(
            MessageKind::System,
            Some("secret system prompt and environment context".into()),
        );
        context.subtype = Some("developer_instructions".into());
        assert!(IngestOp::project_message(context.clone()).is_empty());

        context.usage = Some(TokenSnapshot {
            input: Some(12),
            output: Some(3),
            ..Default::default()
        });
        let ops = IngestOp::project_message(context);
        assert!(matches!(ops.as_slice(), [IngestOp::SessionUsage(_)]));
    }

    #[test]
    fn command_output_becomes_a_typed_notice() {
        let mut output = message(MessageKind::System, Some("Set model to Fable 5.1".into()));
        output.subtype = Some("command_output".into());

        let ops = IngestOp::project_message(output);
        let [IngestOp::Message(entry)] = ops.as_slice() else {
            panic!("expected one command-output notice");
        };
        let TimelineBody::Notice {
            notice: crate::timeline::Notice::CommandOutput { output },
        } = &entry.body
        else {
            panic!("expected typed command output, got {:?}", entry.body);
        };
        assert_eq!(output.as_preview(), "Set model to Fable 5.1");
        assert!(entry.validate().is_ok());
    }
}
