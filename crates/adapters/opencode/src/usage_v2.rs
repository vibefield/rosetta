//! 2.x usage (docs/ARCHITECTURE.md §4.1.3, RESEARCH_PLAN §5.13): one
//! assistant `session_message` row is one step and one model request, a
//! compaction row carries its own request, and title generation is recorded
//! only as a durable `session.usage.recorded.*` event. All 2.x tokens are
//! exclusive as stored (`session/usage.ts:11-19`).

use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_adapter_support::{Drift, Unknown, note_unknown};
use rosetta_core::rows::{SessionUsageRow, TokenSnapshot};
use rosetta_core::{IngestOp, ids};
use serde::Deserialize;
use serde_json::Value;

use crate::normalize::{drift, hard};
use crate::raw::message::{Assistant, Compaction, TokenUsage, UsageRecordedEvent};

#[derive(Deserialize)]
struct UsageRecord {
    event_id: String,
    #[allow(dead_code)]
    seq: i64,
    created: i64,
    event_type: String,
    data: String,
    #[serde(flatten)]
    unknown: Unknown,
}

/// The step's usage lands on its first surviving row, or becomes a request
/// sample when none survived, dated at the step's completion.
pub(crate) fn step_usage(
    ctx: &NormalizeCtx,
    message_id: &str,
    assistant: &Assistant,
    first_row: Option<String>,
    created: i64,
    ops: &mut Vec<IngestOp>,
) {
    let Some(tokens) = &assistant.tokens else {
        return;
    };
    let ts = assistant
        .time
        .completed
        .and_then(|completed| i64::try_from(completed).ok())
        .unwrap_or(created);
    let model = Some(assistant.model.id.clone());
    let tokens = exclusive_snapshot(tokens, Some(&assistant.model.provider_id));
    match first_row {
        Some(response) => ops.push(IngestOp::Usage {
            identity: None,
            session_id: ctx.session_id.clone(),
            response,
            ts,
            model,
            tokens,
        }),
        None => ops.push(IngestOp::SessionUsage(SessionUsageRow {
            identity: None,
            session_id: ctx.session_id.clone(),
            key: ids::usage_id(&ctx.session_id, message_id),
            ts,
            model,
            tokens,
        })),
    }
}

/// The compaction request's own usage (`compaction.ts:435-457,557-573,
/// 609-733`) is a sample: the message that carries it is the summary, not a
/// response. A failed compaction still cost its tokens.
pub(crate) fn compaction_usage(
    ctx: &NormalizeCtx,
    message_id: &str,
    compaction: &Compaction,
    created: i64,
    ops: &mut Vec<IngestOp>,
) {
    let Some(tokens) = &compaction.tokens else {
        return;
    };
    let provider = compaction
        .model
        .as_ref()
        .map(|model| model.provider_id.as_str());
    ops.push(IngestOp::SessionUsage(SessionUsageRow {
        identity: None,
        session_id: ctx.session_id.clone(),
        key: ids::usage_id(&ctx.session_id, message_id),
        ts: created,
        model: compaction.model.as_ref().map(|model| model.id.clone()),
        tokens: exclusive_snapshot(tokens, provider),
    }));
}

/// `session.usage.recorded.*`: title generation is recorded nowhere else and
/// becomes a request sample with no model (the event names none). A
/// compaction's usage is already on its compaction message.
pub(crate) fn usage_event(
    ctx: &mut NormalizeCtx,
    stream_id: &str,
    row: Value,
    outer_paths: Vec<String>,
) -> LineOutcome {
    let record = match serde_json::from_value::<UsageRecord>(row) {
        Ok(record) => record,
        Err(_) => return hard(ctx, stream_id, "snapshot/usage-v2", "parse_failed", None),
    };
    let mut paths = outer_paths;
    note_unknown(&record.unknown, "snapshot.row", &mut paths);
    let kind = format!("event/{}", record.event_type);
    let at = Some(record.created);
    let event = match serde_json::from_str::<UsageRecordedEvent>(&record.data) {
        Ok(event) => event,
        Err(_) => return hard(ctx, stream_id, &kind, "parse_failed", at),
    };
    let mut ops = Vec::new();
    paths.extend(event.drift_paths());
    if !paths.is_empty() {
        ops.push(drift(ctx, stream_id, &kind, "unknown_fields", &paths, at));
    }
    match event.source.as_str() {
        "compaction" => {
            return LineOutcome {
                ops,
                status: LineStatus::Parsed,
            };
        }
        "title" => {}
        other => ops.push(drift(
            ctx,
            stream_id,
            &kind,
            &format!("unknown_usage_source:{other}"),
            &[],
            at,
        )),
    }
    ops.push(IngestOp::SessionUsage(SessionUsageRow {
        identity: None,
        session_id: ctx.session_id.clone(),
        key: ids::usage_id(&ctx.session_id, &format!("event:{}", record.event_id)),
        ts: record.created,
        model: None,
        tokens: exclusive_snapshot(&event.tokens, None),
    }));
    LineOutcome {
        ops,
        status: LineStatus::Parsed,
    }
}

/// 2.x tokens are exclusive as stored: the non-cached prompt, the visible
/// output, reasoning, and the cache reads and writes each land in their own
/// bucket.
fn exclusive_snapshot(tokens: &TokenUsage, provider: Option<&str>) -> TokenSnapshot {
    let exact = crate::usage::exact_i64;
    TokenSnapshot {
        input: exact(&tokens.input),
        output: exact(&tokens.output),
        cache_read: exact(&tokens.cache.read),
        cache_write: exact(&tokens.cache.write),
        thinking: exact(&tokens.reasoning),
        route: provider.and_then(crate::usage::route),
        ..TokenSnapshot::default()
    }
}
