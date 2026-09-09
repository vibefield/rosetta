//! Raw transcript records → canonical ops.
//!
//! The one transform that matters here is the SPLIT: Claude Code can pack text,
//! thinking and tool calls into one assistant record or persist one response
//! across several records. The canonical model wants one row per visible block
//! with response-wide block identity (docs/canonical-model.html, "Message rows").
//! Everything else — tool results folding onto their call and file edits —
//! follows from that flattening. Session cwd is admitted by the bounded skim.
//!
//! Pure by contract (CLAUDE.md): no IO, no clock, no randomness. Every id is
//! derived from the session id plus a stable discriminator, so replaying a file
//! twice produces byte-identical ops.

use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rosetta_adapter_support::tool::{
    canonical_text, lexical_resource, projection_record_kind, projection_shape_paths,
    terminal_update, unknown_interaction,
};
use rosetta_adapter_support::{Drift, LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{
    DriftFact, EditKind, FileEditRow, MessageKind, MessageRow, Ms, SessionUsageRow, TokenSnapshot,
};
use rosetta_core::timeline::{
    AssetRef, AttachmentKind, AttachmentRef, CollaborationChannel, CollaborationEvent,
    CorrelationDerivation, FAILURE_SUMMARY_MAX_BYTES, FailureKind, GroupDerivation, GroupKind,
    ID_MAX_BYTES, Notice, Provenance, ResourceRef, ResponseGroup, StatusBasis, TASK_SNAPSHOT_LIMIT,
    TaskOp, TaskSnapshot, TaskSnapshotItem, TimelineBody, TimelineEntry, TimelineGroup, ToolAction,
    ToolFailure, ToolOutcomePatch, ToolRequestPatch, ToolStatus, ToolStatusFact, ToolTiming,
    ToolUpdatePatch, WorkflowUsage,
};
use rosetta_core::{IngestOp, ProjectionAsset, fold, ids};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::raw::transcript::{
    AiTitleLine, AssistantContentBlock, AssistantLine, CustomTitleLine, KnownAssistantBlock,
    KnownRecord, KnownUserBlock, RecordedUsage, SystemLine, UserContent, UserContentBlock,
    UserLine,
};
use crate::raw::{ParsedLine, parse_line};
use crate::spec;
use crate::time::rfc3339_ms;

/// A record `type` the canonical does not declare — the agent changed under us.
const UNKNOWN_TYPE: &str = "unknown_type";
/// A record type we DO know, whose shape moved. The alarm.
const PARSE_FAILED: &str = "parse_failed";
/// Fields the raw types did not expect, on an otherwise good record.
const UNKNOWN_FIELDS: &str = "unknown_fields";

/// Drift grouping keys for records that cannot name themselves. Angle brackets
/// match the canonical file's own sentinel style (`<unmatched>`, `<str>`).
const NO_TYPE: &str = "<none>";
const INVALID_JSON: &str = "<invalid-json>";

/// Adapter-private per-file response-fold state.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ClaudeCodeState {
    /// Native response ids are not necessarily contiguous in a transcript:
    /// branch reconstruction can repeat an older assistant record thousands
    /// of lines later. Keep the closed response block identity so checkpoint
    /// resume and branch replay assign the same canonical block index.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    responses: BTreeMap<String, ClaudeResponseState>,
    /// Result records carry no tool name. Retain only confirmed Agent/Task
    /// opener ids so an incidental `agentId` field cannot link an unrelated
    /// tool to a child session.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    delegate_calls: BTreeSet<String>,
    /// File result records likewise carry no tool name. Remember only typed
    /// file-change openers so a coincidentally patch-shaped payload from some
    /// other tool can never change its canonical family.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    file_change_calls: BTreeSet<String>,
    /// Task results carry the created id/list state, while updates carry their
    /// intent only on the opener. Keep the bounded typed request until its
    /// terminal record so the final timeline row can hold an after-state.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    task_calls: BTreeMap<String, ToolAction>,
    /// Collaboration acknowledgements carry routing and delivery identity
    /// only in the result record, so retain the exact typed opener by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    collaboration_calls: BTreeMap<String, ToolAction>,
    /// Transient point-in-time task board. This is checkpointed normalizer
    /// state, never a second durable task store; the raw ledger recreates it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tasks: Vec<TaskSnapshotItem>,
    #[serde(default, skip_serializing_if = "is_zero")]
    task_total_count: u64,
    /// Accepted Workflow calls remain addressable until their later
    /// task-notification terminalizes the exact attempt.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workflow_calls: BTreeMap<String, WorkflowCallState>,
    /// Number of distinct accepted invocations observed for each durable run.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workflow_attempts: BTreeMap<String, u64>,
    /// Non-Workflow calls stay here only between their opener and either a
    /// normal result or a later async task notification. This lets a machine
    /// callback update the exact act after an `async_launched` receipt.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    notification_calls: BTreeMap<String, NotificationCallState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowCallState {
    action: ToolAction,
    started_at: Option<Ms>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NotificationCallState {
    started_at: Option<Ms>,
    background: bool,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl ClaudeCodeState {
    /// Claude may persist one API response as several assistant JSONL records,
    /// each containing only newly materialized blocks. A branch replay may then
    /// repeat a prior record with the same UUID and a different parent. Stable
    /// block identities make that a last-writer update, not a new response slot.
    fn enter_response(&mut self, id: &str, block_identities: &[String]) -> ResponseProjection {
        let response = self.responses.entry(id.to_owned()).or_default();
        let mut block_indices = Vec::with_capacity(block_identities.len());
        for identity in block_identities {
            let index = response
                .blocks
                .iter()
                .position(|known| known == identity)
                .and_then(|index| u32::try_from(index).ok())
                .unwrap_or_else(|| {
                    let index = u32::try_from(response.blocks.len()).unwrap_or(u32::MAX);
                    response.blocks.push(identity.clone());
                    index
                });
            block_indices.push(index);
        }
        ResponseProjection {
            block_indices,
            usage: response.usage.clone(),
        }
    }

    fn remember_usage(&mut self, response_id: &str, usage: ClaudeResponseUsage) {
        if let Some(response) = self.responses.get_mut(response_id) {
            response.usage = Some(usage);
        }
    }

    fn remember_delegate_call(&mut self, tool_use_id: &str) {
        self.delegate_calls.insert(tool_use_id.to_owned());
    }

    fn take_delegate_call(&mut self, tool_use_id: &str) -> bool {
        self.delegate_calls.remove(tool_use_id)
    }

    fn remember_file_change_call(&mut self, tool_use_id: &str) {
        self.file_change_calls.insert(tool_use_id.to_owned());
    }

    fn take_file_change_call(&mut self, tool_use_id: &str) -> bool {
        self.file_change_calls.remove(tool_use_id)
    }

    fn remember_task_call(&mut self, tool_use_id: &str, action: &ToolAction) {
        self.task_calls
            .insert(tool_use_id.to_owned(), action.clone());
    }

    fn remember_collaboration_call(&mut self, tool_use_id: &str, action: &ToolAction) {
        self.collaboration_calls
            .insert(tool_use_id.to_owned(), action.clone());
    }

    fn take_collaboration_call(&mut self, tool_use_id: &str) -> Option<ToolAction> {
        self.collaboration_calls.remove(tool_use_id)
    }

    fn remember_workflow_call(
        &mut self,
        tool_use_id: &str,
        action: &ToolAction,
        started_at: Option<Ms>,
    ) {
        self.workflow_calls
            .entry(tool_use_id.to_owned())
            .or_insert_with(|| WorkflowCallState {
                action: action.clone(),
                started_at,
            });
    }

    fn workflow_call(&self, tool_use_id: &str) -> Option<WorkflowCallState> {
        self.workflow_calls.get(tool_use_id).cloned()
    }

    fn update_workflow_call(&mut self, tool_use_id: &str, action: ToolAction) {
        if let Some(call) = self.workflow_calls.get_mut(tool_use_id) {
            call.action = action;
        }
    }

    fn forget_workflow_call(&mut self, tool_use_id: &str) {
        self.workflow_calls.remove(tool_use_id);
    }

    fn workflow_attempt(&mut self, tool_use_id: &str, run_id: &str) -> u64 {
        if let Some(WorkflowCallState {
            action:
                ToolAction::Workflow {
                    attempt: Some(attempt),
                    run_id: Some(known_run),
                    ..
                },
            ..
        }) = self.workflow_calls.get(tool_use_id)
            && known_run == run_id
        {
            return *attempt;
        }
        let attempt = self.workflow_attempts.entry(run_id.to_owned()).or_default();
        *attempt = attempt.saturating_add(1);
        *attempt
    }

    fn remember_notification_call(
        &mut self,
        tool_use_id: &str,
        action: &ToolAction,
        started_at: Option<Ms>,
    ) {
        self.notification_calls.insert(
            tool_use_id.to_owned(),
            NotificationCallState {
                started_at,
                background: action_requests_background(action),
            },
        );
        while self.notification_calls.len() > TASK_NOTIFICATION_CALL_LIMIT {
            self.notification_calls.pop_first();
        }
    }

    fn notification_call(&self, tool_use_id: &str) -> Option<NotificationCallState> {
        self.notification_calls.get(tool_use_id).cloned()
    }

    fn forget_notification_call(&mut self, tool_use_id: &str) {
        self.notification_calls.remove(tool_use_id);
    }

    fn resolve_task_call(
        &mut self,
        tool_use_id: &str,
        result: Option<&Value>,
        failed: bool,
    ) -> Option<ToolAction> {
        let mut action = self.task_calls.remove(tool_use_id)?;
        if failed {
            return None;
        }

        let changed_task_id = self.apply_task_result(&mut action, result)?;
        let snapshot = self.task_snapshot(changed_task_id.as_deref());
        if let ToolAction::Task {
            snapshot: action_snapshot,
            ..
        } = &mut action
        {
            *action_snapshot = Some(snapshot);
        }
        Some(action)
    }

    /// Apply the successful native operation to the transient board. `Some`
    /// means an after-state is honest; the inner id names the changed row when
    /// there is one. A malformed create/list result returns `None` rather than
    /// drawing a board that pretends the operation was represented.
    fn apply_task_result(
        &mut self,
        action: &mut ToolAction,
        result: Option<&Value>,
    ) -> Option<Option<String>> {
        let ToolAction::Task {
            operation,
            task_id,
            subject,
            status,
            owner,
            blocker_ids,
            ..
        } = action
        else {
            return None;
        };

        match operation {
            TaskOp::Create => {
                let created = result
                    .and_then(|value| value.get("task"))
                    .and_then(crate::tool::project_task_item)?;
                // A successful create cannot refer to an existing task. Claude
                // reuses its small numeric ids whenever it starts a fresh task
                // board (including more than once in a single JSONL session),
                // so an id collision is the only native reset marker we get.
                // Clear the old namespace before drawing the new after-state;
                // otherwise completed work from an earlier board leaks into it.
                if self
                    .tasks
                    .iter()
                    .any(|task| task.task_id == created.task_id)
                {
                    self.tasks.clear();
                    self.task_total_count = 0;
                }
                task_id.clone_from(&Some(created.task_id.clone()));
                if subject.is_none() {
                    subject.clone_from(&created.subject);
                }
                if status.is_none() {
                    status.clone_from(&created.status);
                }
                if owner.is_none() {
                    owner.clone_from(&created.owner);
                }
                if blocker_ids.is_empty() {
                    blocker_ids.clone_from(&created.blocker_ids);
                }
                let item = TaskSnapshotItem {
                    task_id: created.task_id.clone(),
                    subject: subject.clone().or(created.subject),
                    status: (*status).or(created.status),
                    owner: owner.clone().or(created.owner),
                    blocker_ids: if blocker_ids.is_empty() {
                        created.blocker_ids
                    } else {
                        blocker_ids.clone()
                    },
                };
                self.upsert_task(item);
                Some(Some(created.task_id))
            }
            TaskOp::Update => {
                if task_id.is_none() {
                    *task_id = result
                        .and_then(|value| value.get("taskId"))
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned);
                }
                if status.is_none() {
                    *status = result
                        .and_then(|value| value.get("statusChange"))
                        .and_then(|change| change.get("to"))
                        .and_then(Value::as_str)
                        .and_then(crate::tool::task_status);
                }
                let id = task_id.clone()?;
                let mut item = self
                    .tasks
                    .iter()
                    .find(|task| task.task_id == id)
                    .cloned()
                    .unwrap_or(TaskSnapshotItem {
                        task_id: id.clone(),
                        subject: None,
                        status: None,
                        owner: None,
                        blocker_ids: Vec::new(),
                    });
                if subject.is_some() {
                    item.subject.clone_from(subject);
                }
                if status.is_some() {
                    item.status = *status;
                }
                if owner.is_some() {
                    item.owner.clone_from(owner);
                }
                for blocker in blocker_ids.iter() {
                    if !item.blocker_ids.contains(blocker) {
                        item.blocker_ids.push(blocker.clone());
                    }
                }
                if subject.is_none() {
                    subject.clone_from(&item.subject);
                }
                if owner.is_none() {
                    owner.clone_from(&item.owner);
                }
                self.upsert_task(item);
                Some(Some(id))
            }
            TaskOp::List => {
                let (tasks, total_count) = crate::tool::project_task_list_result(result?)?;
                self.tasks = tasks.into_iter().take(TASK_STATE_LIMIT).collect();
                self.task_total_count = total_count;
                Some(None)
            }
            TaskOp::Get => {
                if let Some(item) = result
                    .and_then(|value| value.get("task").or(Some(value)))
                    .and_then(crate::tool::project_task_item)
                {
                    let id = item.task_id.clone();
                    if subject.is_none() {
                        subject.clone_from(&item.subject);
                    }
                    if status.is_none() {
                        *status = item.status;
                    }
                    if owner.is_none() {
                        owner.clone_from(&item.owner);
                    }
                    self.upsert_task(item);
                    if task_id.is_none() {
                        *task_id = Some(id.clone());
                    }
                    Some(Some(id))
                } else if self.tasks.is_empty() {
                    None
                } else {
                    Some(task_id.clone())
                }
            }
            TaskOp::Stop | TaskOp::Output => None,
        }
    }

    fn upsert_task(&mut self, item: TaskSnapshotItem) {
        if let Some(existing) = self
            .tasks
            .iter_mut()
            .find(|task| task.task_id == item.task_id)
        {
            *existing = item;
            return;
        }
        self.task_total_count = self.task_total_count.saturating_add(1);
        if self.tasks.len() < TASK_STATE_LIMIT {
            self.tasks.push(item);
        }
    }

    fn task_snapshot(&self, changed_task_id: Option<&str>) -> TaskSnapshot {
        let mut tasks: Vec<_> = self
            .tasks
            .iter()
            .take(TASK_SNAPSHOT_LIMIT)
            .cloned()
            .collect();
        if let Some(changed_task_id) = changed_task_id
            && !tasks.iter().any(|task| task.task_id == changed_task_id)
            && let Some(changed) = self
                .tasks
                .iter()
                .find(|task| task.task_id == changed_task_id)
                .cloned()
        {
            if tasks.len() == TASK_SNAPSHOT_LIMIT {
                tasks.pop();
            }
            tasks.push(changed);
        }
        TaskSnapshot {
            tasks,
            total_count: self
                .task_total_count
                .max(u64::try_from(self.tasks.len()).unwrap_or(u64::MAX)),
        }
    }
}

const TASK_STATE_LIMIT: usize = 512;
const TASK_NOTIFICATION_CALL_LIMIT: usize = 1_024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ClaudeResponseState {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<ClaudeResponseUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ClaudeResponseUsage {
    snapshot: TokenSnapshot,
    target: ClaudeUsageTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ClaudeUsageTarget {
    Timeline { timeline_id: String },
    Session { sample_key: String },
}

struct ResponseProjection {
    block_indices: Vec<u32>,
    usage: Option<ClaudeResponseUsage>,
}

/// One raw line in, canonical ops out. See [`crate::adapter::ClaudeCodeAdapter`].
pub fn ingest(ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard_error(ctx, stream_id, INVALID_JSON, None);
    };
    if text.trim().is_empty() {
        return LineOutcome::parsed(Vec::new());
    }

    match parse_line(text) {
        Ok(ParsedLine::Known(record)) => normalize(ctx, stream_id, &record),
        Ok(ParsedLine::UnknownType { rtype, value }) => {
            let kind = rtype.as_deref().unwrap_or(NO_TYPE);
            // A type the canonical declares is handled — we simply project
            // nothing from it. Only an UNDECLARED type is drift, otherwise the
            // most common record in the corpus (`attachment`) would raise the
            // alarm on every ingest.
            if spec::declared_transcript_types().contains(kind) {
                let paths = spec::unexpected_declared_fields(kind, &value);
                if paths.is_empty() {
                    return LineOutcome::parsed(Vec::new());
                }
                let mut emit = Emit::new(ctx, stream_id);
                emit.drift(
                    UNKNOWN_FIELDS,
                    kind,
                    paths,
                    record_ts(&value),
                    record_version(&value),
                );
                return LineOutcome::parsed(emit.ops);
            }
            let mut emit = Emit::new(ctx, stream_id);
            emit.drift(
                UNKNOWN_TYPE,
                kind,
                top_level_keys(&value),
                record_ts(&value),
                record_version(&value),
            );
            LineOutcome {
                ops: emit.ops,
                status: LineStatus::UnknownType,
            }
        }
        // parse_line fails on malformed JSON, or on a KNOWN type whose required
        // shape moved — that second case is the drift the raw types exist to
        // catch, so it is a hard error rather than a shrug.
        Err(_) => match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                let kind = value["type"].as_str().unwrap_or(NO_TYPE).to_owned();
                hard_error(ctx, stream_id, &kind, Some(&value))
            }
            Err(_) => hard_error(ctx, stream_id, INVALID_JSON, None),
        },
    }
}

fn normalize(ctx: &mut NormalizeCtx, stream_id: &str, record: &KnownRecord) -> LineOutcome {
    let mut emit = Emit::new(ctx, stream_id);
    match record {
        KnownRecord::User(line) => user_line(&mut emit, line),
        KnownRecord::Assistant(line) => assistant_line(&mut emit, line),
        KnownRecord::System(line) => system_line(&mut emit, line),
        KnownRecord::AiTitle(line) => ai_title_line(&mut emit, line),
        KnownRecord::CustomTitle(line) => custom_title_line(&mut emit, line),
        // Session bookkeeping the canonical model has no row for: the resume
        // latch, the prompt queue, the last-prompt pointer, the mode marker.
        // None of them carries a uuid or a timestamp, so none could be placed
        // on a timeline even if we wanted it there.
        KnownRecord::AtisLatch(_)
        | KnownRecord::QueueOperation(_)
        | KnownRecord::LastPrompt(_)
        | KnownRecord::Mode(_) => {}
    }

    let paths = record.drift_paths();
    if !paths.is_empty() {
        let (kind, at, version) = record_identity(record);
        emit.drift(UNKNOWN_FIELDS, kind, paths, at, version);
    }
    LineOutcome::parsed(emit.ops)
}

// ---------------------------------------------------------------- record kinds

fn user_line(emit: &mut Emit, line: &UserLine) {
    // Claude persists model-only turn companions as `user` records. They are
    // injected skill/context reminders (including the `[Image: ...]` sentence
    // emitted after image reads), not words the human authored. Projecting
    // one as a user row invents a prompt boundary and makes the transcript
    // split one reply into several fake turns. The paired flags are the native
    // provenance; content matching would mistake a literal human sentence for
    // transport.
    if line.is_meta == Some(true)
        && line.turn_companion == Some(true)
        && line.is_compact_summary != Some(true)
    {
        return;
    }

    let ts = rfc3339_ms(&line.timestamp);
    let seed = Seed {
        uuid: Some(&line.uuid),
        parent_uuid: line.parent_uuid.as_deref(),
        ts,
        version: Some(&line.version),
        model: None,
        effort: None,
    };

    // The CLI stores its generated continuation recap with the API `user`
    // role. The native flag, not the recap's boilerplate, proves compaction.
    // Preserve its text verbatim before interpreting any command/team markup
    // quoted inside it, and retain the parent link to its compact boundary.
    if line.is_compact_summary == Some(true) {
        let mut text = String::new();
        match &line.message.content {
            UserContent::Text(content) => text.push_str(content),
            UserContent::Blocks(blocks) => {
                for block in blocks {
                    if let UserContentBlock::Known(KnownUserBlock::Text { text: block, .. }) = block
                    {
                        push_text(&mut text, block);
                    }
                }
            }
        }
        let mut row = emit.row(&seed, MessageKind::Summary, &line.uuid);
        row.subtype = Some("compact_summary".to_owned());
        row.content = Some(text);
        row.group = Some(TimelineGroup {
            id: line.uuid.clone(),
            kind: GroupKind::SystemCycle,
            derivation: GroupDerivation::Native,
        });
        emit.push_row(row);
        return;
    }

    let mut text = String::new();
    let mut results: Vec<ToolResultBlock<'_>> = Vec::new();
    let mut attachments = Vec::new();
    let mut assets = Vec::new();
    let mut has_other_block = false;
    match &line.message.content {
        UserContent::Text(prompt) => match local_command_artifact(prompt) {
            LocalCommandArtifact::Caveat | LocalCommandArtifact::EmptyOutput => return,
            LocalCommandArtifact::Invocation(command) => text.push_str(&command),
            LocalCommandArtifact::Output(output) => {
                push_command_output(emit, &seed, &line.uuid, output);
                return;
            }
            LocalCommandArtifact::None => text.push_str(prompt),
        },
        UserContent::Blocks(blocks) => {
            for block in blocks {
                match block {
                    UserContentBlock::Known(KnownUserBlock::Text { text: block, .. }) => {
                        push_text(&mut text, block);
                    }
                    UserContentBlock::Known(KnownUserBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        ..
                    }) => results.push(ToolResultBlock {
                        tool_use_id,
                        content: content.as_ref(),
                        is_error: *is_error,
                        cwd: Some(&line.cwd),
                        tool_use_result: line.tool_use_result.as_ref(),
                        delegate_child_native_id: line
                            .tool_use_result
                            .as_ref()
                            .and_then(|result| {
                                ["agentId", "agent_id", "teammate_id"]
                                    .into_iter()
                                    .find_map(|key| result.get(key).and_then(Value::as_str))
                            })
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned),
                    }),
                    UserContentBlock::Known(KnownUserBlock::Image { source, .. }) => {
                        match authored_attachment(AttachmentKind::Image, source) {
                            Some((attachment, asset)) => {
                                attachments.push(attachment);
                                assets.extend(asset);
                            }
                            None => has_other_block = true,
                        }
                    }
                    UserContentBlock::Known(KnownUserBlock::Document { source, .. }) => {
                        match authored_attachment(AttachmentKind::Document, source) {
                            Some((attachment, asset)) => {
                                attachments.push(attachment);
                                assets.extend(asset);
                            }
                            None => has_other_block = true,
                        }
                    }
                    UserContentBlock::Other(_) => has_other_block = true,
                }
            }
        }
    }

    // The API-shaped `user` role also carries coordinator assignments, peer
    // deliveries, and top-level Agent Team envelopes. Classify those before
    // the human-message boundary; nested envelopes stay inside their owning
    // notification body and are never replayed as separate speech.
    let collaboration = if attachments.is_empty() && !has_other_block && !text.is_empty() {
        if let Some(notification) = crate::envelope::task_notification(&text) {
            let has_event = notification.task_id.is_some()
                || notification.tool_use_id.is_some()
                || notification.status.is_some()
                || notification.summary.is_some();
            if has_event && !emit.task_notification(notification.clone(), ts) {
                emit.push_async_task_notice(&seed, &line.uuid, notification);
            }
            // A recognized callback is transport, never human-authored prose.
            Some(Vec::new())
        } else {
            crate::envelope::classify(
                &text,
                line.origin.as_ref(),
                line.team_name.as_deref(),
                emit.stream_id == spec::SUBAGENT_TRANSCRIPT,
            )
        }
    } else {
        None
    };

    if let Some(drafts) = collaboration {
        // A single delivered envelope can retain the exact sender tool id on
        // its containing user record. Multiple sibling envelopes cannot all
        // own that one id, so leave those unresolved instead of collapsing
        // distinct speech.
        let source_delivery_id = (drafts.len() == 1)
            .then(|| line.source_tool_use_id.clone())
            .flatten();
        for (index, mut draft) in drafts.into_iter().enumerate() {
            if draft.delivery_id.is_none() {
                draft.delivery_id = source_delivery_id.clone();
            }
            emit.push_collaboration(
                &seed,
                &format!("{}#collaboration-{index}", line.uuid),
                draft,
            );
        }
    // A turn made only of tool_results is the report-back for a tool call — it
    // folds into that call's row instead of becoming a row of its own.
    } else if !text.is_empty() || !attachments.is_empty() || has_other_block || results.is_empty() {
        let mut row = emit.row(&seed, MessageKind::User, &line.uuid);
        row.content = (!text.is_empty()).then_some(text);
        row.attachments = attachments;
        row.group = Some(TimelineGroup {
            id: line.uuid.clone(),
            kind: GroupKind::HumanTurn,
            derivation: GroupDerivation::Native,
        });
        emit.ops.extend(
            assets
                .into_iter()
                .map(|asset| IngestOp::ProjectionAsset { asset }),
        );
        emit.push_row(row);
    }

    for (index, result) in results.into_iter().enumerate() {
        emit.tool_result(&result, u32::try_from(index).unwrap_or(u32::MAX), ts);
    }
}

fn assistant_line(emit: &mut Emit, line: &AssistantLine) {
    let ts = rfc3339_ms(&line.timestamp);
    let seed = Seed {
        uuid: Some(&line.uuid),
        parent_uuid: line.parent_uuid.as_deref(),
        ts,
        version: Some(&line.version),
        model: Some(&line.message.model),
        effort: line.effort.as_deref(),
    };

    let mut rows: Vec<(MessageRow, Option<FileEditRow>)> = Vec::new();
    let mut block_identities = Vec::new();
    for (index, block) in line.message.content.iter().enumerate() {
        // Split rows share the record's uuid, so the block index is what makes
        // the timeline id unique within the session.
        let discriminator = format!("{}#{index}", line.uuid);
        match block {
            AssistantContentBlock::Known(KnownAssistantBlock::Text { text, .. })
                if !text.trim().is_empty() =>
            {
                let mut row = emit.row(&seed, MessageKind::Agent, &discriminator);
                row.content = Some(text.clone());
                rows.push((row, None));
                block_identities.push(discriminator);
            }
            AssistantContentBlock::Known(KnownAssistantBlock::Thinking { thinking, .. })
                if !thinking.trim().is_empty() =>
            {
                let mut row = emit.row(&seed, MessageKind::Thinking, &discriminator);
                row.content = Some(thinking.clone());
                rows.push((row, None));
                block_identities.push(discriminator);
            }
            AssistantContentBlock::Known(KnownAssistantBlock::ToolUse {
                id, name, input, ..
            }) => {
                let mut row = emit.row(&seed, MessageKind::ToolUse, &discriminator);
                let projected = crate::tool::project_request(
                    Some(&line.version),
                    name,
                    input,
                    Some(&line.cwd),
                    id,
                    Some(id),
                    None,
                );
                let (tool, is_delegate, is_file_change, is_task, is_collaboration, is_workflow) =
                    match projected {
                        Ok(tool) => {
                            let is_delegate = matches!(&tool.action, ToolAction::Delegate { .. });
                            let is_file_change =
                                matches!(&tool.action, ToolAction::FileChange { .. });
                            let is_task = matches!(&tool.action, ToolAction::Task { .. });
                            let is_collaboration =
                                matches!(&tool.action, ToolAction::Collaborate { .. });
                            let is_workflow = matches!(&tool.action, ToolAction::Workflow { .. });
                            (
                                tool,
                                is_delegate,
                                is_file_change,
                                is_task,
                                is_collaboration,
                                is_workflow,
                            )
                        }
                        Err(error) => {
                            emit.drift(
                                error.drift_code(),
                                &projection_record_kind("request", None, name),
                                projection_shape_paths(input),
                                ts,
                                Some(&line.version),
                            );
                            (
                                unknown_interaction(
                                    id,
                                    CorrelationDerivation::NativeId,
                                    Some(id.clone()),
                                    None,
                                    name,
                                    None,
                                ),
                                false,
                                false,
                                false,
                                false,
                                false,
                            )
                        }
                    };
                if is_task {
                    emit.remember_task_call(id, &tool.action);
                }
                if is_collaboration {
                    emit.remember_collaboration_call(id, &tool.action);
                }
                if is_workflow {
                    emit.remember_workflow_call(id, &tool.action, ts);
                } else {
                    emit.remember_notification_call(id, &tool.action, ts);
                }
                row.tool = Some(tool);
                // A native name is not enough to establish canonical
                // delegation. In particular, an unpinned writer is projected
                // as Unknown; remembering that opener as a delegate would make
                // its later result an invalid cross-family update.
                if is_delegate {
                    row.branch_tool_id = Some(id.clone());
                    emit.remember_delegate_call(id);
                }
                if is_file_change {
                    emit.remember_file_change_call(id);
                }
                let edit = file_edit(&row, name, input, ts);
                rows.push((row, edit));
                block_identities.push(discriminator);
            }
            // Empty prose/reasoning and `fallback` (an in-response model switch)
            // carry no user-visible timeline content. Unfamiliar kinds land in
            // `Other` and are reported by the drift pass.
            _ => {}
        }
    }

    let fallback_first_block = rows
        .first()
        .and_then(|(row, _)| u32::try_from(row.ordinal).ok())
        .unwrap_or_default();
    let response =
        emit.response_projection(&line.message.id, &block_identities, fallback_first_block);
    for ((row, _), block_index) in rows.iter_mut().zip(&response.block_indices) {
        row.group = Some(TimelineGroup {
            id: line.message.id.clone(),
            kind: GroupKind::AgentCycle,
            derivation: GroupDerivation::Native,
        });
        row.response = Some(ResponseGroup {
            id: line.message.id.clone(),
            derivation: GroupDerivation::Native,
        });
        row.block_index = Some(*block_index);
    }

    // Claude repeats the same final response usage on every persisted block.
    // The first observation owns one canonical fact; an actual correction is
    // emitted as a replacement update against that same target.
    let mut deferred_usage = None;
    if let Some(tokens) = line
        .message
        .usage
        .as_ref()
        .map(snapshot)
        .filter(|tokens| !tokens.is_empty())
        && response.usage.as_ref().map(|usage| usage.snapshot) != Some(tokens)
    {
        let target = response
            .usage
            .as_ref()
            .map(|usage| usage.target.clone())
            .unwrap_or_else(|| response_usage_target(&rows, &line.message.id));
        match &target {
            ClaudeUsageTarget::Timeline { timeline_id } if response.usage.is_none() => {
                if let Some((row, _)) = rows
                    .iter_mut()
                    .find(|(row, _)| row.timeline_id == *timeline_id)
                {
                    row.usage = Some(tokens);
                }
            }
            ClaudeUsageTarget::Timeline { timeline_id } => {
                deferred_usage = Some(IngestOp::Usage {
                    session_id: emit.session_id.clone(),
                    response: timeline_id.clone(),
                    ts: ts.unwrap_or(0),
                    model: Some(line.message.model.clone()),
                    tokens,
                });
            }
            ClaudeUsageTarget::Session { sample_key } => {
                deferred_usage = Some(IngestOp::SessionUsage(SessionUsageRow {
                    session_id: emit.session_id.clone(),
                    key: sample_key.clone(),
                    ts: ts.unwrap_or(0),
                    model: Some(line.message.model.clone()),
                    tokens,
                }));
            }
        }
        emit.remember_response_usage(
            &line.message.id,
            ClaudeResponseUsage {
                snapshot: tokens,
                target,
            },
        );
    }
    for (row, edit) in rows {
        emit.push_row(row);
        if let Some(edit) = edit {
            emit.ops.push(IngestOp::FileEdit(edit));
        }
    }
    if let Some(usage) = deferred_usage {
        emit.ops.push(usage);
    }
}

fn system_line(emit: &mut Emit, line: &SystemLine) {
    let ts = rfc3339_ms(&line.timestamp);
    let seed = Seed {
        uuid: Some(&line.uuid),
        parent_uuid: line.parent_uuid.as_deref(),
        ts,
        version: Some(&line.version),
        model: None,
        effort: None,
    };
    if line.subtype == "local_command" {
        let Some(content) = line.content.as_deref() else {
            return;
        };
        match local_command_artifact(content) {
            LocalCommandArtifact::Caveat | LocalCommandArtifact::EmptyOutput => return,
            LocalCommandArtifact::Invocation(command) => {
                let mut row = emit.row(&seed, MessageKind::User, &line.uuid);
                row.content = Some(command);
                row.group = Some(TimelineGroup {
                    id: line.uuid.clone(),
                    kind: GroupKind::HumanTurn,
                    derivation: GroupDerivation::Native,
                });
                emit.push_row(row);
            }
            LocalCommandArtifact::Output(output) => {
                push_command_output(emit, &seed, &line.uuid, output);
            }
            LocalCommandArtifact::None => {
                push_command_output(emit, &seed, &line.uuid, content.to_owned());
            }
        }
        return;
    }

    let mut row = emit.row(&seed, MessageKind::System, &line.uuid);
    row.subtype = Some(line.subtype.clone());
    row.content = line.content.clone();
    emit.push_row(row);
}

/// The title is a session singleton the CLI rewrites, and the record carries no
/// uuid to key it by. Every ai-title therefore folds onto ONE row — a later
/// title upserts over the earlier one instead of stacking up.
fn ai_title_line(emit: &mut Emit, line: &AiTitleLine) {
    let seed = Seed {
        uuid: None,
        parent_uuid: None,
        ts: None,
        version: None,
        model: None,
        effort: None,
    };
    let mut row = emit.row(&seed, MessageKind::Summary, "ai-title");
    row.subtype = Some("ai_title".to_owned());
    row.content = Some(line.ai_title.clone());
    emit.push_row(row);
}

fn custom_title_line(emit: &mut Emit, line: &CustomTitleLine) {
    let seed = Seed {
        uuid: None,
        parent_uuid: None,
        ts: None,
        version: None,
        model: None,
        effort: None,
    };
    let mut row = emit.row(&seed, MessageKind::Summary, "custom-title");
    row.subtype = Some("custom_title".to_owned());
    row.content = Some(line.custom_title.clone());
    emit.push_row(row);
}

// -------------------------------------------------------------------- derived

/// Pick the best row already available when usage is first observed. A response
/// split across JSONL records cannot be buffered waiting for a later text block,
/// so the first projectable record becomes the stable replacement target.
fn response_usage_target(
    rows: &[(MessageRow, Option<FileEditRow>)],
    response_id: &str,
) -> ClaudeUsageTarget {
    let target = rows
        .iter()
        .position(|(row, _)| row.kind == MessageKind::Agent)
        .or_else(|| {
            rows.iter()
                .position(|(row, _)| row.kind == MessageKind::ToolUse)
        })
        .unwrap_or(0);
    match rows.get(target) {
        Some((row, _)) => ClaudeUsageTarget::Timeline {
            timeline_id: row.timeline_id.clone(),
        },
        None => ClaudeUsageTarget::Session {
            sample_key: format!("response:{response_id}"),
        },
    }
}

/// Anthropic's `input_tokens` already EXCLUDES the cached read, so the
/// components map across one-to-one — no inclusive-counter split here (that is
/// codex's rule, `fold::split_inclusive_input`).
fn snapshot(usage: &RecordedUsage) -> TokenSnapshot {
    TokenSnapshot {
        input: i64::try_from(usage.input_tokens).ok(),
        output: i64::try_from(usage.output_tokens).ok(),
        cache_read: i64::try_from(usage.cache_read_input_tokens).ok(),
        cache_write: i64::try_from(usage.cache_creation_input_tokens).ok(),
        thinking: usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| details["thinking_tokens"].as_i64()),
    }
}

/// Claude's three file-changing tools also feed project activity. Edit's input
/// shape is canonical (`file_path/old_string/new_string/replace_all`); Write
/// and NotebookEdit are read defensively — a change we cannot measure is still
/// recorded, with the unknown side of the line count left absent.
fn file_edit(row: &MessageRow, name: &str, input: &Value, at: Option<Ms>) -> Option<FileEditRow> {
    let (kind, (lines_added, lines_removed)) = match name {
        "Edit" => (
            EditKind::Edit,
            input["new_string"].as_str().map_or((None, None), |new| {
                fold::line_delta(input["old_string"].as_str(), new)
            }),
        ),
        "Write" => (
            EditKind::Write,
            input["content"]
                .as_str()
                .map_or((None, None), |content| fold::line_delta(None, content)),
        ),
        "NotebookEdit" => (
            EditKind::Edit,
            input["new_source"]
                .as_str()
                .map_or((None, None), |source| fold::line_delta(None, source)),
        ),
        _ => return None,
    };
    Some(FileEditRow {
        session_id: row.session_id.clone(),
        message_timeline_id: Some(row.timeline_id.clone()),
        path: input["file_path"]
            .as_str()
            .or_else(|| input["notebook_path"].as_str())?
            .to_owned(),
        kind,
        // Claude Code keeps its own backups under file-history/; wiring that up
        // is that stream's job, not this one's.
        snapshot_ref: None,
        lines_added,
        lines_removed,
        at,
    })
}

/// A tool_result block's display text. The canonical observes only strings, but
/// the API also allows a block array (text + images), so both are read.
fn result_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                if let Some(part) = block["text"].as_str() {
                    push_text(&mut text, part);
                }
            }
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

#[derive(Debug)]
struct WorkflowLaunchDraft {
    task_id: Option<String>,
    run_id: String,
    name: Option<String>,
    summary: Option<String>,
    script_path: Option<String>,
}

fn workflow_launch_draft(
    native: Option<&Value>,
    display_text: Option<&str>,
) -> Option<WorkflowLaunchDraft> {
    let native_launch = native.filter(|value| {
        value.get("status").and_then(Value::as_str) == Some("async_launched")
            && value
                .get("taskType")
                .or_else(|| value.get("task_type"))
                .and_then(Value::as_str)
                .is_none_or(|kind| kind == "local_workflow")
    });
    let value = |keys: &[&str]| {
        native_launch.and_then(|native| {
            keys.iter().find_map(|key| {
                native
                    .get(*key)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
            })
        })
    };
    let line = |prefix: &str| {
        display_text.and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix(prefix).map(str::trim))
                .filter(|text| !text.is_empty())
        })
    };
    let run_id = value(&["runId", "run_id"])
        .or_else(|| line("Run ID:"))?
        .to_owned();
    if native_launch.is_none()
        && !display_text.is_some_and(|text| text.contains("Workflow launched in background"))
    {
        return None;
    }
    Some(WorkflowLaunchDraft {
        task_id: value(&["taskId", "task_id"])
            .or_else(|| line("Task ID:"))
            .map(str::to_owned),
        run_id,
        name: value(&["workflowName", "workflow_name"]).map(str::to_owned),
        summary: value(&["summary"])
            .or_else(|| line("Summary:"))
            .map(str::to_owned),
        script_path: value(&["scriptPath", "script_path"])
            .or_else(|| line("Script file:"))
            .map(str::to_owned),
    })
}

fn bounded_utf8(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes.saturating_sub(3).min(value.len());
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AuthoredSource {
    Base64 {
        media_type: String,
        data: String,
    },
    Url {
        url: String,
    },
    Text {
        data: String,
        #[serde(default)]
        media_type: Option<String>,
    },
}

/// Strictly decode only user-authored attachment sources. Tool-result media
/// never calls this path. Invalid labels or bytes stay recoverable in the raw
/// ledger and do not create a projection member that would fail the batch.
fn authored_attachment(
    kind: AttachmentKind,
    source: &Value,
) -> Option<(AttachmentRef, Option<ProjectionAsset>)> {
    match serde_json::from_value::<AuthoredSource>(source.clone()).ok()? {
        AuthoredSource::Url { url } if !url.is_empty() => Some((
            AttachmentRef {
                kind,
                name: None,
                media_type: None,
                byte_len: None,
                source: ResourceRef::Url { url },
                dimensions: None,
            },
            None,
        )),
        AuthoredSource::Base64 { media_type, data } => {
            let media_type = canonical_media_type(&media_type)?;
            if kind == AttachmentKind::Image && !media_type.starts_with("image/") {
                return None;
            }
            let bytes = STANDARD.decode(data).ok()?;
            if bytes.is_empty() || !member_magic_matches(media_type, &bytes) {
                return None;
            }
            member_attachment(kind, media_type, bytes)
        }
        AuthoredSource::Text { data, media_type } if kind == AttachmentKind::Document => {
            if data.is_empty()
                || media_type.as_deref().is_some_and(|value| {
                    canonical_media_type(value) != Some("text/plain; charset=utf-8")
                })
            {
                return None;
            }
            member_attachment(kind, "text/plain; charset=utf-8", data.into_bytes())
        }
        _ => None,
    }
}

fn member_attachment(
    kind: AttachmentKind,
    media_type: &str,
    bytes: Vec<u8>,
) -> Option<(AttachmentRef, Option<ProjectionAsset>)> {
    let asset = ProjectionAsset::new(media_type, bytes)?;
    let reference = AssetRef {
        url: asset.url()?,
        digest: asset.digest.clone(),
        media_type: asset.media_type.clone(),
        byte_len: u64::try_from(asset.bytes.len()).ok()?,
    };
    Some((
        AttachmentRef {
            kind,
            name: None,
            media_type: Some(asset.media_type.clone()),
            byte_len: Some(reference.byte_len),
            source: ResourceRef::Asset { asset: reference },
            dimensions: None,
        },
        Some(asset),
    ))
}

fn canonical_media_type(media_type: &str) -> Option<&'static str> {
    match media_type.to_ascii_lowercase().as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "application/pdf" => Some("application/pdf"),
        "text/plain" | "text/plain; charset=utf-8" => Some("text/plain; charset=utf-8"),
        _ => None,
    }
}

fn member_magic_matches(media_type: &str, bytes: &[u8]) -> bool {
    match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "text/plain; charset=utf-8" => std::str::from_utf8(bytes).is_ok(),
        _ => false,
    }
}

// --------------------------------------------------------------------- plumbing

struct ToolResultBlock<'a> {
    tool_use_id: &'a str,
    content: Option<&'a Value>,
    is_error: Option<bool>,
    cwd: Option<&'a str>,
    tool_use_result: Option<&'a Value>,
    delegate_child_native_id: Option<String>,
}

fn task_notification_status(value: Option<&str>) -> ToolStatus {
    match value {
        Some("completed" | "succeeded" | "success") => ToolStatus::Succeeded,
        Some("failed" | "error") => ToolStatus::Failed,
        Some("timed_out" | "timeout") => ToolStatus::TimedOut,
        Some("killed" | "stopped" | "interrupted") => ToolStatus::Interrupted,
        Some("cancelled" | "canceled") => ToolStatus::Cancelled,
        Some("running" | "pending" | "queued") => ToolStatus::Running,
        _ => ToolStatus::Unknown,
    }
}

fn task_notification_terminal(status: ToolStatus) -> bool {
    !matches!(status, ToolStatus::Pending | ToolStatus::Running)
}

fn action_requests_background(action: &ToolAction) -> bool {
    matches!(
        action,
        ToolAction::Shell {
            background: true,
            ..
        } | ToolAction::Delegate {
            background: true,
            ..
        }
    )
}

fn task_result_is_async(result: &ToolResultBlock<'_>) -> bool {
    let native = result.tool_use_result;
    let status = native
        .and_then(|value| value.get("status").or_else(|| value.get("state")))
        .and_then(Value::as_str);
    if matches!(
        status,
        Some("async_launched" | "running" | "pending" | "queued")
    ) {
        return true;
    }
    if native
        .and_then(|value| value.get("isAsync").or_else(|| value.get("is_async")))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return true;
    }
    result_text(result.content).is_some_and(|text| {
        let text = text.to_ascii_lowercase();
        text.contains("launched in background") || text.contains("running in background")
    })
}

/// The per-line fields every row from that line inherits.
struct Seed<'a> {
    uuid: Option<&'a str>,
    parent_uuid: Option<&'a str>,
    ts: Option<Ms>,
    version: Option<&'a str>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
}

struct Emit<'a> {
    ctx: &'a mut NormalizeCtx,
    stream_id: &'a str,
    session_id: String,
    ops: Vec<IngestOp>,
}

impl<'a> Emit<'a> {
    fn new(ctx: &'a mut NormalizeCtx, stream_id: &'a str) -> Self {
        let session_id = ctx.session_id.clone();
        Self {
            ctx,
            stream_id,
            session_id,
            ops: Vec::new(),
        }
    }

    fn row(&mut self, seed: &Seed, kind: MessageKind, discriminator: &str) -> MessageRow {
        MessageRow {
            timeline_id: ids::timeline_id(&self.session_id, discriminator),
            session_id: self.session_id.clone(),
            ordinal: self.ctx.take_ordinal(),
            kind,
            subtype: None,
            uuid: seed.uuid.map(str::to_owned),
            parent_uuid: seed.parent_uuid.map(str::to_owned),
            ts: seed.ts,
            model: seed.model.map(str::to_owned),
            effort: seed.effort.map(str::to_owned),
            content: None,
            tool: None,
            attachments: Vec::new(),
            citations: Vec::new(),
            group: None,
            response: None,
            block_index: None,
            branch_tool_id: None,
            usage: None,
            // Claude Code stamps the writing CLI version on every record, which
            // beats the runtime-wide version the pipeline guessed.
            agent_version: seed
                .version
                .map(str::to_owned)
                .or_else(|| self.ctx.agent_version.clone()),
            src_cursor: self.ctx.src_cursor,
            src_offset: self.ctx.src_offset,
        }
    }

    fn push_row(&mut self, row: MessageRow) {
        IngestOp::push_message(&mut self.ops, row);
    }

    fn push_collaboration(
        &mut self,
        seed: &Seed,
        discriminator: &str,
        draft: crate::envelope::CollaborationDraft,
    ) {
        let row = self.row(seed, MessageKind::System, discriminator);
        let (body, asset) = match draft.body {
            Some(body) => {
                let (body, asset) = rosetta_core::CanonicalText::externalize(body);
                (Some(body), asset)
            }
            None => (None, None),
        };
        let group_kind = if draft.channel == CollaborationChannel::Team {
            GroupKind::SystemCycle
        } else {
            GroupKind::ParentInvocation
        };
        let entry = TimelineEntry {
            timeline_id: row.timeline_id.clone(),
            session_id: row.session_id,
            ordinal: row.ordinal,
            group: TimelineGroup {
                id: discriminator.to_owned(),
                kind: group_kind,
                derivation: GroupDerivation::Native,
            },
            response: None,
            block_index: None,
            native_id: row.uuid,
            parent_native_id: row.parent_uuid,
            at: row.ts,
            model: None,
            effort: None,
            body: TimelineBody::Collaboration {
                event: Box::new(CollaborationEvent {
                    kind: draft.kind,
                    channel: draft.channel,
                    sender: draft.sender,
                    recipient: draft.recipient,
                    team: draft.team,
                    color: draft.color,
                    summary: draft.summary,
                    body,
                    task_id: draft.task_id,
                    delivery_id: draft.delivery_id,
                    status: draft.status,
                    authored_at: draft.authored_at,
                    source_session_id: None,
                    source_response_id: None,
                    work_start_timeline_id: None,
                    work_end_timeline_id: None,
                }),
            },
            parent_tool_native_id: None,
            usage: TokenSnapshot::default(),
            provenance: Provenance {
                agent_version: row.agent_version,
                src_cursor: row.src_cursor,
                src_offset: row.src_offset,
            },
        };
        if entry.validate().is_err() {
            return;
        }
        if let Some(asset) = asset {
            self.ops.push(IngestOp::ProjectionAsset { asset });
        }
        self.ops.push(IngestOp::Message(Box::new(entry)));
    }

    fn push_async_task_notice(
        &mut self,
        seed: &Seed,
        discriminator: &str,
        notification: crate::envelope::TaskNotificationDraft,
    ) {
        let row = self.row(seed, MessageKind::System, discriminator);
        let entry = TimelineEntry {
            timeline_id: row.timeline_id,
            session_id: row.session_id,
            ordinal: row.ordinal,
            group: TimelineGroup {
                id: discriminator.to_owned(),
                kind: GroupKind::SystemCycle,
                derivation: GroupDerivation::Native,
            },
            response: None,
            block_index: None,
            native_id: row.uuid,
            parent_native_id: row.parent_uuid,
            at: row.ts,
            model: None,
            effort: None,
            body: TimelineBody::Notice {
                notice: Notice::AsyncTask {
                    task_id: notification
                        .task_id
                        .map(|value| bounded_utf8(value, ID_MAX_BYTES)),
                    tool_use_id: notification
                        .tool_use_id
                        .map(|value| bounded_utf8(value, ID_MAX_BYTES)),
                    status: task_notification_status(notification.status.as_deref()),
                    summary: notification
                        .summary
                        .map(|value| bounded_utf8(value, FAILURE_SUMMARY_MAX_BYTES)),
                    duration_ms: notification.duration_ms,
                },
            },
            parent_tool_native_id: None,
            usage: TokenSnapshot::default(),
            provenance: Provenance {
                agent_version: row.agent_version,
                src_cursor: row.src_cursor,
                src_offset: row.src_offset,
            },
        };
        if entry.validate().is_ok() {
            self.ops.push(IngestOp::Message(Box::new(entry)));
        }
    }

    fn response_projection(
        &mut self,
        response_id: &str,
        block_identities: &[String],
        fallback_first_block: u32,
    ) -> ResponseProjection {
        self.ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .map(|state| state.enter_response(response_id, block_identities))
            // `AgentAdapter::init_state` makes this unreachable in production.
            // Keeping a deterministic ordinal fallback makes direct trait use
            // degrade to collision-free output instead of panicking.
            .unwrap_or(ResponseProjection {
                block_indices: (0..block_identities.len())
                    .map(|index| {
                        fallback_first_block
                            .saturating_add(u32::try_from(index).unwrap_or(u32::MAX))
                    })
                    .collect(),
                usage: None,
            })
    }

    fn remember_response_usage(&mut self, response_id: &str, usage: ClaudeResponseUsage) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.remember_usage(response_id, usage);
        }
    }

    fn remember_delegate_call(&mut self, tool_use_id: &str) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.remember_delegate_call(tool_use_id);
        }
    }

    fn take_delegate_call(&mut self, tool_use_id: &str) -> bool {
        self.ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .is_some_and(|state| state.take_delegate_call(tool_use_id))
    }

    fn remember_file_change_call(&mut self, tool_use_id: &str) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.remember_file_change_call(tool_use_id);
        }
    }

    fn take_file_change_call(&mut self, tool_use_id: &str) -> bool {
        self.ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .is_some_and(|state| state.take_file_change_call(tool_use_id))
    }

    fn remember_task_call(&mut self, tool_use_id: &str, action: &ToolAction) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.remember_task_call(tool_use_id, action);
        }
    }

    fn remember_collaboration_call(&mut self, tool_use_id: &str, action: &ToolAction) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.remember_collaboration_call(tool_use_id, action);
        }
    }

    fn remember_workflow_call(
        &mut self,
        tool_use_id: &str,
        action: &ToolAction,
        started_at: Option<Ms>,
    ) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.remember_workflow_call(tool_use_id, action, started_at);
        }
    }

    fn remember_notification_call(
        &mut self,
        tool_use_id: &str,
        action: &ToolAction,
        started_at: Option<Ms>,
    ) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.remember_notification_call(tool_use_id, action, started_at);
        }
    }

    fn notification_call(&mut self, tool_use_id: &str) -> Option<NotificationCallState> {
        self.ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .and_then(|state| state.notification_call(tool_use_id))
    }

    fn forget_notification_call(&mut self, tool_use_id: &str) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.forget_notification_call(tool_use_id);
        }
    }

    fn workflow_call(&mut self, tool_use_id: &str) -> Option<WorkflowCallState> {
        self.ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .and_then(|state| state.workflow_call(tool_use_id))
    }

    fn update_workflow_call(&mut self, tool_use_id: &str, action: ToolAction) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.update_workflow_call(tool_use_id, action);
        }
    }

    fn forget_workflow_call(&mut self, tool_use_id: &str) {
        if let Some(state) = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
        {
            state.forget_workflow_call(tool_use_id);
        }
    }

    fn resolve_workflow_launch(
        &mut self,
        result: &ToolResultBlock<'_>,
    ) -> Option<(ToolAction, Option<Ms>)> {
        let display = result_text(result.content);
        let draft = workflow_launch_draft(result.tool_use_result, display.as_deref())?;
        let call = self.workflow_call(result.tool_use_id)?;
        let attempt = self
            .ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .map(|state| state.workflow_attempt(result.tool_use_id, &draft.run_id))?;
        let mut action = call.action;
        let ToolAction::Workflow {
            name,
            path,
            objective,
            resume_id,
            background,
            task_id,
            run_id,
            attempt: action_attempt,
            ..
        } = &mut action
        else {
            return None;
        };
        if let Some(value) = draft.name {
            *name = Some(bounded_utf8(value, rosetta_core::timeline::ID_MAX_BYTES));
        }
        if let Some(value) = draft.script_path {
            *path = lexical_resource(&value, result.cwd, false).ok();
        }
        if objective.is_none()
            && let Some(value) = draft.summary
            && let Ok(value) = canonical_text(value)
        {
            *objective = Some(value);
        }
        *background = true;
        *task_id = draft.task_id;
        *run_id = Some(draft.run_id.clone());
        *action_attempt = Some(attempt);
        if attempt > 1 && resume_id.is_none() {
            *resume_id = Some(draft.run_id);
        }
        self.update_workflow_call(result.tool_use_id, action.clone());
        Some((action, call.started_at))
    }

    fn workflow_notification(
        &mut self,
        notification: crate::envelope::TaskNotificationDraft,
        at: Option<Ms>,
    ) -> bool {
        let Some(tool_use_id) = notification.tool_use_id.as_deref() else {
            return false;
        };
        let Some(call) = self.workflow_call(tool_use_id) else {
            return false;
        };
        let mut action = call.action;
        let ToolAction::Workflow {
            task_id,
            callback_summary,
            callback_result,
            usage,
            ..
        } = &mut action
        else {
            return false;
        };
        if notification.task_id.is_some() {
            *task_id = notification.task_id;
        }
        *callback_summary = notification
            .summary
            .map(|summary| bounded_utf8(summary, FAILURE_SUMMARY_MAX_BYTES));
        *callback_result = notification
            .result
            .and_then(|result| canonical_text(result).ok())
            .map(Box::new);
        let callback_usage = WorkflowUsage {
            duration_ms: notification.duration_ms,
            tokens: notification.tokens,
            tool_calls: notification.tool_uses,
            agent_count: notification.agent_count,
        };
        if callback_usage != WorkflowUsage::default() {
            *usage = Some(callback_usage.clone());
        }

        let status = task_notification_status(notification.status.as_deref());
        let terminal = task_notification_terminal(status);
        let failure = (status == ToolStatus::Failed).then(|| ToolFailure {
            kind: FailureKind::Provider,
            summary: callback_summary.clone(),
            diagnostic: None,
        });
        let patch = ToolUpdatePatch {
            request: Some(ToolRequestPatch {
                action: action.clone(),
            }),
            status: Some(ToolStatusFact {
                value: status,
                basis: StatusBasis::GroundTruthProtocol,
            }),
            failure,
            timing: Some(ToolTiming {
                started_at: call.started_at,
                completed_at: terminal.then_some(at).flatten(),
                duration_ms: callback_usage.duration_ms,
            }),
            ..ToolUpdatePatch::default()
        };
        if terminal {
            self.forget_workflow_call(tool_use_id);
        } else {
            self.update_workflow_call(tool_use_id, action);
        }
        IngestOp::push_tool_update(
            &mut self.ops,
            self.session_id.clone(),
            tool_use_id.to_owned(),
            self.ctx.evidence(0),
            patch,
            at,
        );
        true
    }

    fn task_notification(
        &mut self,
        notification: crate::envelope::TaskNotificationDraft,
        at: Option<Ms>,
    ) -> bool {
        if self.workflow_notification(notification.clone(), at) {
            return true;
        }
        let Some(tool_use_id) = notification.tool_use_id.as_deref() else {
            return false;
        };
        let Some(call) = self.notification_call(tool_use_id) else {
            return false;
        };
        let status = task_notification_status(notification.status.as_deref());
        let terminal = task_notification_terminal(status);
        let summary = notification
            .summary
            .as_deref()
            .map(|value| bounded_utf8(value.to_owned(), FAILURE_SUMMARY_MAX_BYTES));
        let failure = match status {
            ToolStatus::Failed => Some(ToolFailure {
                kind: FailureKind::Provider,
                summary: summary.clone(),
                diagnostic: None,
            }),
            ToolStatus::TimedOut => Some(ToolFailure {
                kind: FailureKind::Timeout,
                summary: summary.clone(),
                diagnostic: None,
            }),
            _ => None,
        };
        let patch = ToolUpdatePatch {
            status: Some(ToolStatusFact {
                value: status,
                basis: StatusBasis::GroundTruthProtocol,
            }),
            failure,
            timing: Some(ToolTiming {
                started_at: call.started_at,
                completed_at: terminal.then_some(at).flatten(),
                duration_ms: notification.duration_ms,
            }),
            ..ToolUpdatePatch::default()
        };
        if terminal {
            self.forget_notification_call(tool_use_id);
        }
        IngestOp::push_tool_update(
            &mut self.ops,
            self.session_id.clone(),
            tool_use_id.to_owned(),
            self.ctx.evidence(0),
            patch,
            at,
        );
        true
    }

    fn resolve_task_call(
        &mut self,
        tool_use_id: &str,
        result: Option<&Value>,
        failed: bool,
    ) -> Option<ToolAction> {
        self.ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .and_then(|state| state.resolve_task_call(tool_use_id, result, failed))
    }

    fn take_collaboration_call(&mut self, tool_use_id: &str) -> Option<ToolAction> {
        self.ctx
            .state
            .as_mut()
            .and_then(|state| state.downcast_mut::<ClaudeCodeState>())
            .and_then(|state| state.take_collaboration_call(tool_use_id))
    }

    fn tool_result(&mut self, result: &ToolResultBlock<'_>, mutation_index: u32, at: Option<Ms>) {
        let native_failure = result
            .tool_use_result
            .and_then(|value| value.get("success"))
            .and_then(Value::as_bool)
            == Some(false);
        let effective_error = if native_failure {
            Some(true)
        } else {
            result.is_error
        };
        let failed = effective_error == Some(true);
        let diagnostic = failed.then(|| result_text(result.content)).flatten();
        let notification_call = self.notification_call(result.tool_use_id);
        let notification_continues = notification_call
            .as_ref()
            .is_some_and(|call| call.background)
            || task_result_is_async(result);
        if self.workflow_call(result.tool_use_id).is_some() {
            if !failed && let Some((action, started_at)) = self.resolve_workflow_launch(result) {
                let patch = ToolUpdatePatch {
                    request: Some(ToolRequestPatch { action }),
                    status: Some(ToolStatusFact {
                        value: ToolStatus::Running,
                        basis: StatusBasis::GroundTruthProtocol,
                    }),
                    timing: Some(ToolTiming {
                        started_at,
                        completed_at: None,
                        duration_ms: None,
                    }),
                    ..ToolUpdatePatch::default()
                };
                IngestOp::push_tool_update(
                    &mut self.ops,
                    self.session_id.clone(),
                    result.tool_use_id.to_owned(),
                    self.ctx.evidence(mutation_index),
                    patch,
                    at,
                );
                return;
            }
            if failed {
                self.forget_workflow_call(result.tool_use_id);
            }
        }
        let mut patch = terminal_update(
            effective_error,
            true,
            StatusBasis::NativeTerminalRecord,
            at,
            diagnostic.as_deref(),
        );
        if notification_continues && !failed {
            patch.status = Some(ToolStatusFact {
                value: ToolStatus::Running,
                basis: StatusBasis::GroundTruthProtocol,
            });
            patch.timing = Some(ToolTiming {
                started_at: notification_call.and_then(|call| call.started_at),
                completed_at: None,
                duration_ms: None,
            });
        }
        if self.take_delegate_call(result.tool_use_id) {
            let child_session_id = result
                .delegate_child_native_id
                .as_deref()
                .map(|native| ids::session_id(&self.ctx.runtime_id, native));
            patch.delegate_child_session_id = child_session_id.clone();
            if !failed
                && let Some(native) = result.tool_use_result
                && let Ok(action) = crate::tool::project_delegate_result(native, child_session_id)
            {
                patch.outcome = Some(ToolOutcomePatch { action });
            }
        }
        if self.take_file_change_call(result.tool_use_id)
            && !failed
            && let Some(action) = result
                .tool_use_result
                .and_then(|value| crate::tool::project_change_result(value, result.cwd).ok())
                .flatten()
        {
            patch.request = Some(ToolRequestPatch { action });
        }
        if let Some(action) =
            self.resolve_task_call(result.tool_use_id, result.tool_use_result, failed)
        {
            patch.request = Some(ToolRequestPatch { action });
        }
        if let Some(request) = self.take_collaboration_call(result.tool_use_id)
            && !failed
            && let Some(result) = result.tool_use_result
            && let Some(action) =
                crate::tool::project_collaboration_result(&request, result, &self.ctx.runtime_id)
        {
            patch.request = Some(ToolRequestPatch {
                action: action.clone(),
            });
            patch.outcome = Some(ToolOutcomePatch { action });
        }
        if failed || !notification_continues {
            self.forget_notification_call(result.tool_use_id);
        }
        IngestOp::push_tool_update(
            &mut self.ops,
            self.session_id.clone(),
            result.tool_use_id.to_owned(),
            self.ctx.evidence(mutation_index),
            patch,
            at,
        );
    }

    fn drift(
        &mut self,
        code: &str,
        record_kind: &str,
        paths: Vec<String>,
        at: Option<Ms>,
        version: Option<&str>,
    ) {
        self.ops.push(IngestOp::Drift(DriftFact {
            runtime_id: self.ctx.runtime_id.clone(),
            agent_version: version
                .map(str::to_owned)
                .or_else(|| self.ctx.agent_version.clone())
                .unwrap_or_default(),
            stream: self.stream_id.to_owned(),
            record_kind: record_kind.to_owned(),
            error_code: code.to_owned(),
            shape_hash: ids::shape_hash(&paths),
            at,
            sample_cursor: self.ctx.src_cursor,
            sample_offset: self.ctx.src_offset,
        }));
    }
}

fn hard_error(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    value: Option<&Value>,
) -> LineOutcome {
    let mut emit = Emit::new(ctx, stream_id);
    emit.drift(
        PARSE_FAILED,
        record_kind,
        value.map(top_level_keys).unwrap_or_default(),
        value.and_then(record_ts),
        value.and_then(record_version),
    );
    LineOutcome {
        ops: emit.ops,
        status: LineStatus::HardError {
            code: PARSE_FAILED.to_owned(),
        },
    }
}

fn record_identity(record: &KnownRecord) -> (&'static str, Option<Ms>, Option<&str>) {
    match record {
        KnownRecord::User(line) => (
            "user",
            rfc3339_ms(&line.timestamp),
            Some(line.version.as_str()),
        ),
        KnownRecord::Assistant(line) => (
            "assistant",
            rfc3339_ms(&line.timestamp),
            Some(line.version.as_str()),
        ),
        KnownRecord::System(line) => (
            "system",
            rfc3339_ms(&line.timestamp),
            Some(line.version.as_str()),
        ),
        KnownRecord::AiTitle(_) => ("ai-title", None, None),
        KnownRecord::CustomTitle(_) => ("custom-title", None, None),
        KnownRecord::AtisLatch(_) => ("atis-latch", None, None),
        KnownRecord::QueueOperation(line) => ("queue-operation", rfc3339_ms(&line.timestamp), None),
        KnownRecord::LastPrompt(_) => ("last-prompt", None, None),
        KnownRecord::Mode(_) => ("mode", None, None),
    }
}

fn top_level_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

fn record_ts(value: &Value) -> Option<Ms> {
    rfc3339_ms(value["timestamp"].as_str()?)
}

fn record_version(value: &Value) -> Option<&str> {
    value["version"].as_str()
}

fn push_text(buffer: &mut String, text: &str) {
    if !buffer.is_empty() {
        buffer.push('\n');
    }
    buffer.push_str(text);
}

enum LocalCommandArtifact {
    None,
    Caveat,
    Invocation(String),
    Output(String),
    EmptyOutput,
}

fn local_command_artifact(text: &str) -> LocalCommandArtifact {
    let trimmed = text.trim();
    if tagged_value(trimmed, "local-command-caveat").is_some() {
        return LocalCommandArtifact::Caveat;
    }
    if let Some(output) = tagged_value(trimmed, "local-command-stdout") {
        let output = concise_local_command_output(plain_terminal_text(output));
        return if output.is_empty() || output == "(no content)" {
            LocalCommandArtifact::EmptyOutput
        } else {
            LocalCommandArtifact::Output(output)
        };
    }
    if trimmed.starts_with("<command-name>") || trimmed.starts_with("<command-message>") {
        let name = tagged_value(trimmed, "command-name")
            .or_else(|| tagged_value(trimmed, "command-message"))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let args = tagged_value(trimmed, "command-args")
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(name) = name {
            let mut command = if name.starts_with('/') {
                name.to_owned()
            } else {
                format!("/{name}")
            };
            if let Some(args) = args {
                command.push(' ');
                command.push_str(args);
            }
            return LocalCommandArtifact::Invocation(command);
        }
    }
    LocalCommandArtifact::None
}

/// Claude explains successful setting changes as UI copy. The canonical row
/// keeps the setting's value, which is the durable fact; scope/default prose
/// and the effort-level description belong to the CLI that rendered them.
/// Unrecognised output (including invalid-command diagnostics) stays intact.
fn concise_local_command_output(output: String) -> String {
    let first_line = output.lines().next().unwrap_or_default().trim();

    if let Some(rest) = first_line.strip_prefix("Set model to ") {
        let mut value = rest.trim();
        for suffix in [
            " and saved as your default for new sessions",
            " and saved as default",
            " for this session",
        ] {
            if let Some(trimmed) = value.strip_suffix(suffix) {
                value = trimmed.trim();
            }
        }
        if let Some(trimmed) = value.strip_suffix(" (default)") {
            value = trimmed.trim();
        }
        value = value.trim_matches('`').trim();
        if !value.is_empty() {
            return value.to_owned();
        }
    }

    if let Some(rest) = first_line.strip_prefix("Set effort level to ") {
        let value = rest
            .split(|ch: char| ch.is_whitespace() || ch == '(' || ch == ':')
            .next()
            .unwrap_or_default()
            .trim();
        if !value.is_empty()
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return value.to_owned();
        }
    }

    output
}

fn tagged_value<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

fn plain_terminal_text(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            if ch != '\r' {
                plain.push(ch);
            }
            continue;
        }

        match chars.next() {
            // Control Sequence Introducer: consume through its final byte.
            Some('[') => {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            // Operating System Command: BEL or String Terminator closes it.
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.next_if_eq(&'\\').is_some() {
                        break;
                    }
                }
            }
            // Other two-byte escape sequences end at their second byte.
            Some(_) | None => {}
        }
    }
    plain.trim().to_owned()
}

fn push_command_output(emit: &mut Emit, seed: &Seed<'_>, discriminator: &str, output: String) {
    let mut row = emit.row(seed, MessageKind::System, discriminator);
    row.subtype = Some("command_output".to_owned());
    row.content = Some(output);
    emit.push_row(row);
}
