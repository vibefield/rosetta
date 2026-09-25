//! Usage accounting for Kimi wires (ARCHITECTURE §4.1.3, RESEARCH_PLAN §5.13).
//!
//! One `usage.record` per finished model request, written by
//! `LlmRequesterService` after the stream's finish event (kimi@103495669,
//! bundle 2.0.2) and before the step's content parts, with `usageScope:
//! "turn"` for loop steps and `"session"` for every other request
//! (`UsageAgentModel.record` kimi@101743598). Its buckets are exclusive by
//! construction: `inputOther = prompt_tokens − cached_tokens`
//! (`parseOpenAIUsage` kimi@98617262). The OpenAI-compatible providers fold
//! cache writes into `inputOther` and never split reasoning out of `output`,
//! so neither can be recovered here.
//!
//! Three rules decide where a turn record lands:
//! - a copy is not usage: a record older than a proven fork instant keeps its
//!   rows and drops its tokens (counting rule 2);
//! - the first visible row of the same step owns the request: a text part
//!   with content, a media part, or a tool call — never a think part, whose
//!   empty variant projects to no row at all;
//! - whatever is still pending when the step closes becomes a sample keyed
//!   by the step uuid, so no request is stranded on an invisible row or
//!   handed to the next step.

use rosetta_adapter_support::{LineStatus, NormalizeCtx};
use rosetta_core::rows::{MessageKind, MessageRow, Ms, SessionUsageRow, TokenSnapshot};
use rosetta_core::{IngestOp, fold, ids};

use crate::normalize::{KimiCodeState, UNKNOWN_SUBTYPE, drift, hard_into, source_discriminator};
use crate::raw::wire::{ContextAppendLoopEventLine, RecordedUsage, TurnEndedLine, UsageRecordLine};
use crate::time::epoch_ms;

/// A native subset larger than its total (here a negative `inputOther`) is
/// clamped at zero and reported, never a parse failure (§4.1.3).
pub(crate) const USAGE_SUBSET_EXCEEDS_TOTAL: &str = "usage_subset_exceeds_total";

pub(crate) fn usage_record(
    ctx: &NormalizeCtx,
    state: &mut KimiCodeState,
    stream_id: &str,
    line: &UsageRecordLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let at = epoch_ms(line.time);
    let scope = line.usage_scope.as_deref();
    if !matches!(scope, Some("turn" | "session")) {
        ops.push(drift(
            ctx,
            stream_id,
            &format!("usage.record/{}", scope.unwrap_or("<none>")),
            UNKNOWN_SUBTYPE,
            &[],
            at,
        ));
        return LineStatus::UnknownType;
    }
    let Some((tokens, clamped)) = usage_snapshot(&line.usage) else {
        return hard_into(
            ctx,
            stream_id,
            "usage.record",
            "usage_out_of_range",
            at,
            ops,
        );
    };
    if clamped {
        ops.push(drift(
            ctx,
            stream_id,
            "usage.record",
            USAGE_SUBSET_EXCEEDS_TOTAL,
            &[],
            at,
        ));
    }
    // The record repeats the configured alias. It names the model only when
    // the request's own `llm.request` is missing or did not parse.
    let model = state
        .model
        .get_or_insert_with(|| line.model.clone())
        .clone();
    // A fork carries the source's records with their original times: their
    // rows stay, their requests were already counted in the source.
    if state.inherited_copy(at) {
        return LineStatus::Parsed;
    }
    match scope {
        // One finished loop request; the step's first visible row owns it.
        // Two requests before one row accumulate into an aggregate rather
        // than the second overwriting the first.
        Some("turn") => {
            state.pending_usage = Some(match state.pending_usage.take() {
                Some(pending) => fold::bucket_apply(&pending, &tokens),
                None => tokens,
            });
        }
        // Every request that is not a loop step (compaction today) is also
        // one billable request, never a running total (RESEARCH_PLAN §5.13).
        // No response row owns it, so it is a replacement-keyed sample.
        _ => {
            let native_key = format!(
                "usage.record:{}:{}:{}",
                line.agent_id.as_deref().unwrap_or(""),
                line.time,
                line.model
            );
            ops.push(IngestOp::SessionUsage(SessionUsageRow {
                identity: None,
                session_id: ctx.session_id.clone(),
                key: ids::usage_id(&ctx.session_id, &native_key),
                ts: at.unwrap_or(0),
                model: Some(model),
                tokens,
            }));
        }
    }
    LineStatus::Parsed
}

/// Stamp the request context on a response row. A row that will be visible
/// in the transcript takes the pending request; a think part never does,
/// so an empty one cannot carry the step's tokens off the timeline.
pub(crate) fn stamp_response(row: &mut MessageRow, state: &mut KimiCodeState) {
    row.model.clone_from(&state.model);
    row.effort.clone_from(&state.effort);
    if owns_usage(row) {
        row.usage = state.pending_usage.take();
    }
}

/// Mirrors `IngestOp::project_message`: the row survives narrowing.
fn owns_usage(row: &MessageRow) -> bool {
    match row.kind {
        MessageKind::ToolUse => row.tool.is_some(),
        MessageKind::Agent => {
            row.content
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
                || !row.attachments.is_empty()
                || !row.citations.is_empty()
        }
        _ => false,
    }
}

/// `step.begin` opens the step whose rows may own the next request. A step
/// that never closed (a crash between `usage.record` and `step.end`) must
/// not hand its request to this step's rows.
pub(crate) fn step_begin(
    ctx: &NormalizeCtx,
    state: &mut KimiCodeState,
    line: &ContextAppendLoopEventLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let key = match state.step.take() {
        Some(previous) => format!("step:{previous}"),
        None => source_discriminator(ctx, "step.begin"),
    };
    flush_pending(ctx, state, key, epoch_ms(line.time), ops);
    state.step.clone_from(&line.event.uuid);
    LineStatus::Parsed
}

/// `step.end` repeats the request's usage (a duplicate rail, never counted)
/// and closes the step: a request no visible row took becomes a sample keyed
/// by the step uuid.
pub(crate) fn step_end(
    ctx: &NormalizeCtx,
    state: &mut KimiCodeState,
    line: &ContextAppendLoopEventLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let key = match line.event.uuid.clone().or_else(|| state.step.take()) {
        Some(step) => format!("step:{step}"),
        None => source_discriminator(ctx, "step.end"),
    };
    state.step = None;
    flush_pending(ctx, state, key, epoch_ms(line.time), ops);
    LineStatus::Parsed
}

/// A turn that ends with a request still pending (no `step.end` was written)
/// closes it the same way.
pub(crate) fn turn_ended(
    ctx: &NormalizeCtx,
    state: &mut KimiCodeState,
    line: &TurnEndedLine,
    ops: &mut Vec<IngestOp>,
) -> LineStatus {
    let key = match state.step.take() {
        Some(step) => format!("step:{step}"),
        None => format!("turn.ended:{}", line.turn_id),
    };
    flush_pending(ctx, state, key, epoch_ms(line.time), ops);
    LineStatus::Parsed
}

fn flush_pending(
    ctx: &NormalizeCtx,
    state: &mut KimiCodeState,
    native_key: String,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let Some(tokens) = state.pending_usage.take() else {
        return;
    };
    ops.push(IngestOp::SessionUsage(SessionUsageRow {
        identity: None,
        session_id: ctx.session_id.clone(),
        key: ids::usage_id(&ctx.session_id, &native_key),
        ts: at.unwrap_or(0),
        model: state.model.clone(),
        tokens,
    }));
}

/// Kimi's usage is exclusive by construction (`inputOther` excludes cache
/// reads and creation). Returns the snapshot and whether `inputOther` had to
/// be clamped from a negative value; `None` when a count is unrepresentable.
pub(crate) fn usage_snapshot(usage: &RecordedUsage) -> Option<(TokenSnapshot, bool)> {
    let clamped = usage.input_other < 0;
    let tokens = TokenSnapshot {
        input: Some(usage.input_other.max(0)),
        output: Some(i64::try_from(usage.output).ok()?),
        cache_read: Some(i64::try_from(usage.input_cache_read).ok()?),
        cache_write: Some(i64::try_from(usage.input_cache_creation).ok()?),
        // Kimi's native usage shape declares no thinking-token component.
        thinking: None,
        ..TokenSnapshot::default()
    };
    Some((tokens, clamped))
}

#[cfg(test)]
mod tests {
    use super::usage_snapshot;
    use crate::raw::wire::RecordedUsage;
    use rosetta_adapter_support::Unknown;

    fn recorded(input_other: i64) -> RecordedUsage {
        RecordedUsage {
            input_other,
            output: 0,
            input_cache_read: 0,
            input_cache_creation: 0,
            unknown: Unknown::new(),
        }
    }

    #[test]
    fn reported_zero_usage_stays_reported() {
        let (tokens, clamped) = usage_snapshot(&recorded(0)).expect("representable usage");
        assert!(!clamped);
        assert_eq!(tokens.input, Some(0));
        assert_eq!(tokens.output, Some(0));
        assert_eq!(tokens.cache_read, Some(0));
        assert_eq!(tokens.cache_write, Some(0));
        assert_eq!(tokens.thinking, None);
    }

    #[test]
    fn a_negative_input_other_clamps_and_reports() {
        // `parseOpenAIUsage` (kimi@98617262) subtracts without a clamp.
        let (tokens, clamped) = usage_snapshot(&recorded(-7)).expect("clamped, not failed");
        assert!(clamped);
        assert_eq!(tokens.input, Some(0));
    }
}
