//! Per-step usage (docs/ARCHITECTURE.md §4.1.3, RESEARCH_PLAN §5.13): one
//! step-finish part is one model request, mapped into the exclusive buckets
//! under the writer version's bucket rules.

use rosetta_adapter_support::adapter::NormalizeCtx;
use rosetta_core::rows::{SessionUsageRow, TokenSnapshot};
use rosetta_core::{IngestOp, fold, ids};

use crate::normalize::{OpenCodeState, TimelineRecord, drift};

/// One step is one model request (RESEARCH_PLAN §5.13): the step-finish part
/// carries exactly that request's tokens, while `message.tokens` keeps only a
/// message's last step. The usage lands on the step's first surviving row, or
/// becomes a request sample when no row owns it. A fork clones its parent's
/// parts after the original message completed, so a step-finish written after
/// its message's completion is a copy the parent already counted.
pub(crate) fn step_usage(
    ctx: &NormalizeCtx,
    stream_id: &str,
    state: &mut OpenCodeState,
    record: &TimelineRecord,
    message: &crate::raw::payload::AssistantMessage,
    step: &crate::raw::payload::StepFinishPart,
    ops: &mut Vec<IngestOp>,
) {
    let target = state.step_rows.remove(&record.message_id).flatten();
    let copied = message
        .time
        .completed
        .and_then(|completed| i64::try_from(completed).ok())
        .is_some_and(|completed| record.part_time_created > completed);
    if copied {
        return;
    }
    let (tokens, clamped) = token_snapshot(
        &step.tokens,
        &message.provider_id,
        WriterRules::for_version(ctx.agent_version.as_deref(), &message.provider_id),
    );
    if clamped {
        ops.push(drift(
            ctx,
            stream_id,
            "assistant/step-finish",
            "usage_subset_exceeds_total",
            &["tokens".to_owned()],
            Some(record.part_time_created),
        ));
    }
    match target.filter(|row| state.claimed_rows.insert(row.clone())) {
        Some(response) => ops.push(IngestOp::Usage {
            session_id: ctx.session_id.clone(),
            response,
            ts: record.part_time_created,
            model: Some(message.model_id.clone()),
            tokens,
        }),
        None => ops.push(IngestOp::SessionUsage(SessionUsageRow {
            session_id: ctx.session_id.clone(),
            key: ids::usage_id(&ctx.session_id, &record.part_id),
            ts: record.part_time_created,
            model: Some(message.model_id.clone()),
            tokens,
        })),
    }
}

/// How a writer version stored its token fields (RESEARCH_PLAN §5.13,
/// bisected over OpenCode tags). From 1.3.16 `input` excludes cache reads and
/// writes and `output` excludes `reasoning`; earlier writers left one or both
/// inclusive, depending on the provider SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WriterRules {
    input_includes_cache: bool,
    output_includes_reasoning: bool,
}

impl WriterRules {
    const EXCLUSIVE: Self = Self {
        input_includes_cache: false,
        output_includes_reasoning: false,
    };

    fn for_version(version: Option<&str>, provider: &str) -> Self {
        let Some(version) = version.and_then(semver) else {
            return Self::EXCLUSIVE;
        };
        let anthropic = matches!(provider, "anthropic" | "amazon-bedrock");
        let google = matches!(provider, "google" | "google-vertex");
        if version < (1, 0, 62) {
            Self {
                input_includes_cache: !anthropic,
                output_includes_reasoning: !google,
            }
        } else if version < (1, 3, 4) {
            Self {
                input_includes_cache: false,
                output_includes_reasoning: !google,
            }
        } else if version < (1, 3, 6) {
            Self {
                input_includes_cache: anthropic,
                output_includes_reasoning: true,
            }
        } else if version < (1, 3, 16) {
            Self {
                input_includes_cache: false,
                output_includes_reasoning: true,
            }
        } else {
            Self::EXCLUSIVE
        }
    }
}

fn semver(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim_start_matches('v').split('.');
    let mut next = || {
        parts
            .next()?
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    };
    Some((next()?, next()?, next()?))
}

/// Map one step's tokens into the exclusive buckets. OpenCode never records
/// the cache TTL (its default Anthropic cache control is the 5-minute TTL),
/// a provider-billed amount, or a service tier; its `cost` is a models.dev
/// estimate. Returns whether a subset exceeded its total.
fn token_snapshot(
    tokens: &crate::raw::payload::Tokens,
    provider: &str,
    rules: WriterRules,
) -> (TokenSnapshot, bool) {
    let cache_read = exact_i64(&tokens.cache.read);
    let cache_write = exact_i64(&tokens.cache.write);
    let (input, input_clamped) = if rules.input_includes_cache {
        fold::exclusive_remainder(exact_i64(&tokens.input), &[cache_read, cache_write])
    } else {
        (exact_i64(&tokens.input), false)
    };
    let (output, thinking, output_clamped) = if rules.output_includes_reasoning {
        fold::split_subset(exact_i64(&tokens.output), exact_i64(&tokens.reasoning))
    } else {
        (
            exact_i64(&tokens.output),
            exact_i64(&tokens.reasoning),
            false,
        )
    };
    let snapshot = TokenSnapshot {
        input,
        output,
        cache_read,
        cache_write,
        thinking,
        route: rosetta_core::rows::canonical_route(provider),
        ..TokenSnapshot::default()
    };
    (snapshot, input_clamped || output_clamped)
}

fn exact_i64(number: &serde_json::Number) -> Option<i64> {
    number
        .as_i64()
        .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_support::adapter::{LineStatus, NormalizeCtx};
    use rosetta_core::IngestOp;
    use serde_json::json;

    use crate::normalize::{OpenCodeState, ingest};
    use crate::spec;

    fn assistant(completed: u64) -> serde_json::Value {
        json!({
            "role": "assistant",
            "agent": "build",
            "cost": 0.5,
            "mode": "build",
            "modelID": "claude-sonnet-4-5",
            "parentID": "msg-user",
            "path": {"cwd": "/workspace", "root": "/workspace"},
            "providerID": "anthropic",
            "time": {"created": 100, "completed": completed},
            // The last step's tokens only; never read.
            "tokens": {"input": 1, "output": 1, "reasoning": 0, "cache": {"read": 0, "write": 0}}
        })
    }

    fn part_line(
        message: &serde_json::Value,
        part_id: &str,
        at: i64,
        part: serde_json::Value,
    ) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "query": spec::TIMELINE_QUERY,
            "row": {
                "message_id": "msg-1",
                "message_data": message.to_string(),
                "part_id": part_id,
                "part_time_created": at,
                "part_data": part.to_string(),
                "first_event_seq": at
            }
        }))
        .unwrap()
    }

    fn step_finish(
        input: u64,
        output: u64,
        reasoning: u64,
        read: u64,
        write: u64,
    ) -> serde_json::Value {
        json!({
            "type": "step-finish",
            "cost": 0,
            "reason": "stop",
            "tokens": {"input": input, "output": output, "reasoning": reasoning,
                       "cache": {"read": read, "write": write}}
        })
    }

    #[test]
    fn each_step_finish_is_one_request_on_its_own_row() {
        let message = assistant(900);
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.agent_version = Some("1.18.22".to_owned());
        let mut state = OpenCodeState::default();
        let mut ops = Vec::new();
        for (id, at, part) in [
            ("p1", 110, json!({"type": "step-start"})),
            ("p2", 120, json!({"type": "text", "text": "first answer"})),
            ("p3", 130, step_finish(10, 20, 5, 1_000, 200)),
            // A second step (a retry) with no row of its own.
            ("p4", 140, json!({"type": "step-start"})),
            ("p5", 150, step_finish(7, 3, 0, 1_000, 0)),
        ] {
            let outcome = ingest(
                &mut ctx,
                &mut state,
                spec::DATABASE,
                &part_line(&message, id, at, part),
            );
            assert_eq!(outcome.status, LineStatus::Parsed);
            ops.extend(outcome.ops);
        }
        let usage: Vec<_> = ops
            .iter()
            .filter_map(|op| match op {
                IngestOp::Usage {
                    response, tokens, ..
                } => Some(("row", response.clone(), tokens.clone())),
                IngestOp::SessionUsage(sample) => {
                    Some(("sample", sample.key.clone(), sample.tokens.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(usage.len(), 2, "{usage:?}");
        let (kind, target, first) = &usage[0];
        assert_eq!(*kind, "row");
        assert_eq!(target, &rosetta_core::ids::timeline_id("session", "p2"));
        // 1.18.22 stores exclusive buckets already.
        assert_eq!(
            (first.input, first.output, first.thinking),
            (Some(10), Some(20), Some(5))
        );
        assert_eq!(
            (first.cache_read, first.cache_write),
            (Some(1_000), Some(200))
        );
        assert_eq!(first.route.as_deref(), Some("anthropic"));
        let (kind, _, second) = &usage[1];
        assert_eq!(*kind, "sample");
        assert_eq!((second.input, second.output), (Some(7), Some(3)));
    }

    #[test]
    fn a_fork_copied_step_finish_carries_no_usage() {
        // The clone is written after the original message completed.
        let message = assistant(900);
        let mut ctx = NormalizeCtx::new("runtime", "session");
        let mut state = OpenCodeState::default();
        let outcome = ingest(
            &mut ctx,
            &mut state,
            spec::DATABASE,
            &part_line(&message, "copied", 5_000, step_finish(10, 20, 5, 0, 0)),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert!(outcome.ops.is_empty(), "{:?}", outcome.ops);
    }

    #[test]
    fn older_writers_had_inclusive_buckets() {
        let tokens: crate::raw::payload::Tokens = serde_json::from_value(json!({
            "input": 1_300, "output": 50, "reasoning": 20, "cache": {"read": 1_000, "write": 200}
        }))
        .unwrap();
        // 1.0.x on an OpenAI-compatible provider: input included cache reads
        // and writes, output included reasoning.
        let rules = super::WriterRules::for_version(Some("1.0.40"), "openrouter");
        let (legacy, clamped) = super::token_snapshot(&tokens, "openrouter", rules);
        assert!(!clamped);
        assert_eq!(
            (legacy.input, legacy.output, legacy.thinking),
            (Some(100), Some(30), Some(20))
        );
        // The pinned writer is exclusive: fields map as stored.
        let rules = super::WriterRules::for_version(Some("1.18.22"), "openrouter");
        let (current, _) = super::token_snapshot(&tokens, "openrouter", rules);
        assert_eq!((current.input, current.output), (Some(1_300), Some(50)));
        assert!(super::WriterRules::for_version(Some("1.3.4"), "anthropic").input_includes_cache);
    }
}
