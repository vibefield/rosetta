//! Per-step usage (docs/ARCHITECTURE.md §4.1.3, RESEARCH_PLAN §5.13): one
//! step-finish part is one model request, mapped into the exclusive buckets.
//! The bucket rule is decided per record from the provider's own `total`
//! whenever the writer stored one, and only otherwise from the writer
//! version's bucket band.

use rosetta_adapter_support::adapter::NormalizeCtx;
use rosetta_core::rows::{SessionUsageRow, TokenSnapshot};
use rosetta_core::{IngestOp, fold, ids};

use crate::normalize::{OpenCodeState, TimelineRecord, drift};
use crate::raw::payload::{AssistantMessage, StepFinishPart, Tokens};

/// One step is one model request (RESEARCH_PLAN §5.13): the step-finish part
/// carries exactly that request's tokens, while `message.tokens` keeps only a
/// message's last step. The usage lands on the step's first surviving row, or
/// becomes a request sample when no row owns it.
///
/// A fork (`Session.fork`, `session.ts:691-732` at 1.18.x) creates its session
/// at fork time and clones the parent's messages with their original
/// `time.created`, so a message created before its own session is a copy the
/// parent already counted. Migrated and imported sessions keep their original
/// creation times and are never mistaken for copies; their step-finish parts
/// carry the import instant instead (`storage/json-migration.ts:301-302`,
/// `cli/cmd/import.ts:213-223`), so usage written after the message
/// completed is dated at the completion.
pub(crate) fn step_usage(
    ctx: &NormalizeCtx,
    stream_id: &str,
    state: &mut OpenCodeState,
    record: &TimelineRecord,
    message: &AssistantMessage,
    step: &StepFinishPart,
    ops: &mut Vec<IngestOp>,
) {
    let target = state.step_rows.remove(&record.message_id).flatten();
    if is_fork_copy(record, message) {
        return;
    }
    let completed = message
        .time
        .completed
        .and_then(|completed| i64::try_from(completed).ok());
    let ts = match completed {
        Some(completed) if record.part_time_created > completed => completed,
        _ => record.part_time_created,
    };
    let (tokens, codes) = snapshot(
        &step.tokens,
        &message.provider_id,
        ctx.agent_version.as_deref(),
    );
    for code in codes {
        ops.push(drift(
            ctx,
            stream_id,
            "assistant/step-finish",
            code,
            &["tokens".to_owned()],
            Some(ts),
        ));
    }
    match target.filter(|row| state.claimed_rows.insert(row.clone())) {
        Some(response) => ops.push(IngestOp::Usage {
            identity: None,
            session_id: ctx.session_id.clone(),
            response,
            ts,
            model: Some(message.model_id.clone()),
            tokens,
        }),
        None => ops.push(IngestOp::SessionUsage(SessionUsageRow {
            identity: None,
            session_id: ctx.session_id.clone(),
            key: ids::usage_id(&ctx.session_id, &record.part_id),
            ts,
            model: Some(message.model_id.clone()),
            tokens,
        })),
    }
}

/// A message created before its session was: a fork copy. Records replayed
/// from a ledger written before the session creation time was captured fall
/// back to the earlier heuristic (a step-finish after the message completed).
fn is_fork_copy(record: &TimelineRecord, message: &AssistantMessage) -> bool {
    let created = i64::try_from(message.time.created).ok();
    match (record.session_time_created, created) {
        (Some(session), Some(created)) => created < session,
        (Some(_), None) => false,
        (None, _) => message
            .time
            .completed
            .and_then(|completed| i64::try_from(completed).ok())
            .is_some_and(|completed| record.part_time_created > completed),
    }
}

/// Map one step's tokens into the exclusive buckets, returning the drift
/// codes the mapping had to raise. OpenCode never records the cache TTL (its
/// default Anthropic cache control is the 5-minute TTL), a provider-billed
/// amount, or a service tier; its `cost` is a models.dev estimate.
pub(crate) fn snapshot(
    tokens: &Tokens,
    provider: &str,
    version: Option<&str>,
) -> (TokenSnapshot, Vec<&'static str>) {
    let (rules, mut codes) = bucket_rules(tokens, provider, version);
    let (snapshot, clamped) = token_snapshot(tokens, provider, rules);
    if clamped {
        codes.push("usage_subset_exceeds_total");
    }
    (snapshot, codes)
}

/// How a writer stored its token fields. From 1.3.16 `input` excludes cache
/// reads and writes and `output` excludes `reasoning`; earlier writers left
/// one or both inclusive, depending on the provider SDK.
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
    const REASONING_INSIDE_OUTPUT: Self = Self {
        input_includes_cache: false,
        output_includes_reasoning: true,
    };
}

/// The rule for one record. `tokens.total` is the SDK's own total
/// (`session.ts:366` at 1.18.x, stored since 1.1.65), so it tells which
/// fields already overlap: the exclusive sum matching it proves exclusive
/// storage, the sum without `reasoning` matching it proves reasoning still
/// sits inside `output`. A total matching neither, or no total at all, leaves
/// the writer-version band, and the former is reported.
fn bucket_rules(
    tokens: &Tokens,
    provider: &str,
    version: Option<&str>,
) -> (WriterRules, Vec<&'static str>) {
    let total = tokens.total.as_ref().and_then(exact_i64);
    if let Some(total) = total {
        let parts = [
            exact_i64(&tokens.input),
            exact_i64(&tokens.cache.read),
            exact_i64(&tokens.cache.write),
            exact_i64(&tokens.output),
        ];
        let without_reasoning: Option<i64> = parts
            .iter()
            .try_fold(0_i64, |sum, part| sum.checked_add((*part)?));
        let exclusive = without_reasoning
            .zip(exact_i64(&tokens.reasoning))
            .and_then(|(sum, reasoning)| sum.checked_add(reasoning));
        if exclusive == Some(total) {
            return (WriterRules::EXCLUSIVE, Vec::new());
        }
        if without_reasoning == Some(total) {
            return (WriterRules::REASONING_INSIDE_OUTPUT, Vec::new());
        }
        let (rules, mut codes) = band(version, provider);
        codes.push("usage_total_unexplained");
        return (rules, codes);
    }
    band(version, provider)
}

/// The writer-version band (RESEARCH_PLAN §5.13, bisected over OpenCode
/// tags). Preview builds are versioned `0.0.0-<channel>-<YYYYMMDDhhmm>`
/// (`packages/script/src/index.ts:34-37`), so they take the band of the
/// latest release cut before their build minute. A version that parses as
/// neither takes the current exclusive rule and is reported.
fn band(version: Option<&str>, provider: &str) -> (WriterRules, Vec<&'static str>) {
    let anthropic = matches!(
        provider,
        "anthropic" | "amazon-bedrock" | "google-vertex-anthropic"
    );
    let google = matches!(provider, "google" | "google-vertex");
    let Some(stage) = version.and_then(band_stage) else {
        return (WriterRules::EXCLUSIVE, vec!["unparseable_writer_version"]);
    };
    let rules = match stage {
        0 => WriterRules {
            input_includes_cache: !anthropic,
            output_includes_reasoning: !google,
        },
        1 => WriterRules {
            input_includes_cache: false,
            output_includes_reasoning: !google,
        },
        2 => WriterRules {
            input_includes_cache: anthropic,
            output_includes_reasoning: true,
        },
        3 => WriterRules::REASONING_INSIDE_OUTPUT,
        _ => WriterRules::EXCLUSIVE,
    };
    (rules, Vec::new())
}

/// Release tags that changed the bucket rule, as versions and as the UTC
/// build minutes (`YYYYMMDDhhmm`) of those tags.
const BAND_VERSIONS: [(u64, u64, u64); 4] = [(1, 0, 62), (1, 3, 4), (1, 3, 6), (1, 3, 16)];
const BAND_MINUTES: [u64; 4] = [202511130013, 202603290100, 202603291802, 202604060345];

fn band_stage(version: &str) -> Option<usize> {
    if let Some(minute) = preview_minute(version) {
        return Some(BAND_MINUTES.iter().filter(|cut| minute >= **cut).count());
    }
    let parsed = semver(version)?;
    Some(BAND_VERSIONS.iter().filter(|cut| parsed >= **cut).count())
}

/// The build minute of a `0.0.0-<channel>-<YYYYMMDDhhmm>` preview version.
fn preview_minute(version: &str) -> Option<u64> {
    let rest = version.trim_start_matches('v').strip_prefix("0.0.0-")?;
    let (_, stamp) = rest.rsplit_once('-')?;
    (stamp.len() == 12 && stamp.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| stamp.parse().ok())
        .flatten()
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
    let parsed = (next()?, next()?, next()?);
    (parsed != (0, 0, 0)).then_some(parsed)
}

/// Apply the rules. Returns whether a subset exceeded its total.
fn token_snapshot(tokens: &Tokens, provider: &str, rules: WriterRules) -> (TokenSnapshot, bool) {
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
        route: route(provider),
        ..TokenSnapshot::default()
    };
    (snapshot, input_clamped || output_clamped)
}

/// The serving route is OpenCode's provider id. A custom provider id may
/// carry a `/` (`my-org/proxy`); only its provider segment is a route, never
/// a residency qualifier.
pub(crate) fn route(provider: &str) -> Option<String> {
    rosetta_core::rows::canonical_route(provider.split('/').next().unwrap_or(provider))
}

pub(crate) fn exact_i64(number: &serde_json::Number) -> Option<i64> {
    number
        .as_i64()
        .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
}

#[cfg(test)]
mod tests {
    use rosetta_adapter_support::adapter::{LineStatus, NormalizeCtx};
    use rosetta_core::IngestOp;
    use rosetta_core::rows::{DriftFact, TokenSnapshot};
    use serde_json::json;

    use crate::normalize::{OpenCodeState, ingest};
    use crate::spec;

    fn assistant(created: u64, completed: Option<u64>) -> serde_json::Value {
        let mut message = json!({
            "role": "assistant",
            "agent": "build",
            "cost": 0.5,
            "mode": "build",
            "modelID": "claude-sonnet-4-5",
            "parentID": "msg-user",
            "path": {"cwd": "/workspace", "root": "/workspace"},
            "providerID": "anthropic",
            "time": {"created": created},
            // The last step's tokens only; never read.
            "tokens": {"input": 1, "output": 1, "reasoning": 0, "cache": {"read": 0, "write": 0}}
        });
        if let Some(completed) = completed {
            message["time"]["completed"] = json!(completed);
        }
        message
    }

    fn part_line(
        message: &serde_json::Value,
        part_id: &str,
        at: i64,
        session_created: Option<i64>,
        part: serde_json::Value,
    ) -> Vec<u8> {
        let mut row = json!({
            "message_id": "msg-1",
            "message_data": message.to_string(),
            "part_id": part_id,
            "part_time_created": at,
            "part_data": part.to_string(),
            "first_event_seq": at
        });
        if let Some(created) = session_created {
            row["session_time_created"] = json!(created);
        }
        serde_json::to_vec(&json!({"query": spec::TIMELINE_QUERY, "row": row})).unwrap()
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

    fn tokens(value: serde_json::Value) -> crate::raw::payload::Tokens {
        serde_json::from_value(value).unwrap()
    }

    type UsageOp = (&'static str, String, TokenSnapshot, i64);

    fn usage_ops(ops: &[IngestOp]) -> Vec<UsageOp> {
        ops.iter()
            .filter_map(|op| match op {
                IngestOp::Usage {
                    response,
                    tokens,
                    ts,
                    ..
                } => Some(("row", response.clone(), tokens.clone(), *ts)),
                IngestOp::SessionUsage(sample) => Some((
                    "sample",
                    sample.key.clone(),
                    sample.tokens.clone(),
                    sample.ts,
                )),
                _ => None,
            })
            .collect()
    }

    fn drift_codes(ops: &[IngestOp]) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                IngestOp::Drift(DriftFact { error_code, .. }) => Some(error_code.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn each_step_finish_is_one_request_on_its_own_row() {
        let message = assistant(100, Some(900));
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
                &part_line(&message, id, at, Some(50), part),
            );
            assert_eq!(outcome.status, LineStatus::Parsed);
            ops.extend(outcome.ops);
        }
        let usage = usage_ops(&ops);
        assert_eq!(usage.len(), 2, "{usage:?}");
        let (kind, target, first, ts) = &usage[0];
        assert_eq!(*kind, "row");
        assert_eq!(target, &rosetta_core::ids::timeline_id("session", "p2"));
        assert_eq!(*ts, 130);
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
        let (kind, _, second, _) = &usage[1];
        assert_eq!(*kind, "sample");
        assert_eq!((second.input, second.output), (Some(7), Some(3)));
        assert!(drift_codes(&ops).is_empty(), "{ops:?}");
    }

    #[test]
    fn a_message_created_before_its_session_is_a_fork_copy() {
        // The clone keeps the original's creation time; the forked session
        // was created later, at fork time.
        let message = assistant(100, Some(900));
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.agent_version = Some("1.18.22".to_owned());
        let mut state = OpenCodeState::default();
        let outcome = ingest(
            &mut ctx,
            &mut state,
            spec::DATABASE,
            &part_line(
                &message,
                "copied",
                5_000,
                Some(4_000),
                step_finish(10, 20, 5, 0, 0),
            ),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert!(outcome.ops.is_empty(), "{:?}", outcome.ops);
    }

    #[test]
    fn an_imported_step_finish_keeps_its_usage_dated_at_completion() {
        // The JSON importer stamps step-finish parts with the import instant,
        // long after the message completed; the session keeps its original
        // creation time, so this is history, not a copy.
        let message = assistant(100, Some(900));
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.agent_version = Some("1.1.20".to_owned());
        let mut state = OpenCodeState::default();
        let outcome = ingest(
            &mut ctx,
            &mut state,
            spec::DATABASE,
            &part_line(
                &message,
                "imported",
                5_000,
                Some(50),
                step_finish(10, 20, 5, 0, 0),
            ),
        );
        assert_eq!(outcome.status, LineStatus::Parsed);
        let usage = usage_ops(&outcome.ops);
        assert_eq!(usage.len(), 1, "{:?}", outcome.ops);
        assert_eq!(usage[0].0, "sample");
        assert_eq!(usage[0].3, 900);
        // No `total` on this writer: the 1.1.x band applies (reasoning
        // inside output).
        assert_eq!(
            (usage[0].2.input, usage[0].2.output, usage[0].2.thinking),
            (Some(10), Some(15), Some(5))
        );
    }

    #[test]
    fn records_without_a_session_time_keep_the_completion_heuristic() {
        let message = assistant(100, Some(900));
        let mut ctx = NormalizeCtx::new("runtime", "session");
        let mut state = OpenCodeState::default();
        let outcome = ingest(
            &mut ctx,
            &mut state,
            spec::DATABASE,
            &part_line(
                &message,
                "copied",
                5_000,
                None,
                step_finish(10, 20, 5, 0, 0),
            ),
        );
        assert!(usage_ops(&outcome.ops).is_empty(), "{:?}", outcome.ops);
    }

    #[test]
    fn the_total_decides_the_bucket_rule_before_the_version_band() {
        // Zen Claude on a 1.3.10 writer: the band says reasoning is inside
        // output, but the SDK total proves the fields are exclusive.
        let exclusive = tokens(json!({
            "total": 1_270, "input": 100, "output": 50, "reasoning": 20,
            "cache": {"read": 1_000, "write": 100}
        }));
        let (snapshot, codes) = super::snapshot(&exclusive, "opencode", Some("1.3.10"));
        assert!(codes.is_empty(), "{codes:?}");
        assert_eq!(
            (snapshot.input, snapshot.output, snapshot.thinking),
            (Some(100), Some(50), Some(20))
        );
        // A total without reasoning: output still contains it.
        let inclusive = tokens(json!({
            "total": 1_250, "input": 100, "output": 50, "reasoning": 20,
            "cache": {"read": 1_000, "write": 100}
        }));
        let (snapshot, codes) = super::snapshot(&inclusive, "openai", Some("1.18.23"));
        assert!(codes.is_empty(), "{codes:?}");
        assert_eq!(
            (snapshot.input, snapshot.output, snapshot.thinking),
            (Some(100), Some(30), Some(20))
        );
        // A total matching neither identity falls back to the band, reported.
        let odd = tokens(json!({
            "total": 999, "input": 100, "output": 50, "reasoning": 20,
            "cache": {"read": 1_000, "write": 100}
        }));
        let (snapshot, codes) = super::snapshot(&odd, "openai", Some("1.18.23"));
        assert_eq!(codes, vec!["usage_total_unexplained"]);
        assert_eq!((snapshot.input, snapshot.output), (Some(100), Some(50)));
    }

    #[test]
    fn older_writers_had_inclusive_buckets() {
        let legacy = tokens(json!({
            "input": 1_300, "output": 50, "reasoning": 20, "cache": {"read": 1_000, "write": 200}
        }));
        // 1.0.x on an OpenAI-compatible provider: input included cache reads
        // and writes, output included reasoning.
        let (snapshot, codes) = super::snapshot(&legacy, "openrouter", Some("1.0.40"));
        assert!(codes.is_empty());
        assert_eq!(
            (snapshot.input, snapshot.output, snapshot.thinking),
            (Some(100), Some(30), Some(20))
        );
        // The pinned writer is exclusive: fields map as stored.
        let (current, codes) = super::snapshot(&legacy, "openrouter", Some("1.18.22"));
        assert!(codes.is_empty());
        assert_eq!((current.input, current.output), (Some(1_300), Some(50)));
        // 1.3.4 on the Anthropic SDK (the Vertex route included) still had
        // cache inside input.
        for provider in ["anthropic", "google-vertex-anthropic"] {
            let (snapshot, _) = super::snapshot(&legacy, provider, Some("1.3.4"));
            assert_eq!(snapshot.input, Some(100), "{provider}");
        }
    }

    #[test]
    fn preview_builds_take_the_band_of_their_build_minute() {
        let legacy = tokens(json!({
            "input": 1_300, "output": 50, "reasoning": 20, "cache": {"read": 1_000, "write": 200}
        }));
        // Built after the 1.3.16 cut: exclusive, nothing reported.
        let (snapshot, codes) =
            super::snapshot(&legacy, "opencode", Some("0.0.0-beta-202608110357"));
        assert!(codes.is_empty(), "{codes:?}");
        assert_eq!((snapshot.input, snapshot.output), (Some(1_300), Some(50)));
        // Built between 1.3.6 and 1.3.16: reasoning inside output.
        let (snapshot, _) = super::snapshot(&legacy, "opencode", Some("0.0.0-dev-202604010000"));
        assert_eq!((snapshot.input, snapshot.output), (Some(1_300), Some(30)));
        // Built before 1.0.62: fully inclusive on a non-Anthropic provider.
        let (snapshot, _) = super::snapshot(&legacy, "openai", Some("0.0.0-dev-202510010000"));
        assert_eq!((snapshot.input, snapshot.output), (Some(100), Some(30)));
        assert_eq!(super::band_stage("0.0.0-beta-202604060345"), Some(4));
        assert_eq!(super::band_stage("0.0.0-beta-202604060344"), Some(3));
    }

    #[test]
    fn unparseable_versions_are_exclusive_and_reported() {
        let legacy = tokens(json!({
            "input": 1_300, "output": 50, "reasoning": 20, "cache": {"read": 1_000, "write": 200}
        }));
        for version in [
            None,
            Some(""),
            Some("dev"),
            Some("0.0.0"),
            Some("0.0.0-beta"),
        ] {
            let (snapshot, codes) = super::snapshot(&legacy, "openai", version);
            assert_eq!(codes, vec!["unparseable_writer_version"], "{version:?}");
            assert_eq!((snapshot.input, snapshot.output), (Some(1_300), Some(50)));
        }
        // A present total still settles the rule without any version.
        let exclusive = tokens(json!({
            "total": 2_570, "input": 1_300, "output": 50, "reasoning": 20,
            "cache": {"read": 1_000, "write": 200}
        }));
        let (_, codes) = super::snapshot(&exclusive, "openai", None);
        assert!(codes.is_empty(), "{codes:?}");
    }

    #[test]
    fn a_custom_provider_route_keeps_only_its_provider_segment() {
        assert_eq!(super::route("my-org/proxy").as_deref(), Some("my-org"));
        assert_eq!(super::route("Anthropic").as_deref(), Some("anthropic"));
        assert_eq!(super::route(""), None);
    }
}
