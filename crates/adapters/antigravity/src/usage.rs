//! Conversation-store snapshot records to canonical usage samples.
//!
//! Antigravity's JSONL transcripts carry no usage. The request grain lives in
//! `conversations/<id>.db`: every cascade step's `metadata` blob carries the
//! `ModelUsageStats` of that step's own model request, and each generation's
//! `gen_metadata` blob names the steps it wrote plus the served model string.
//! One `session_usage` sample is emitted per step with usage, keyed by the
//! step index, dated by the step's own `created_at`, and attributed to the
//! model of the generation that owns the step. Steps no generation claims —
//! the checkpoint summarizer's calls, which persist only a placeholder model
//! enum — keep their tokens with no model and therefore stay unpriced.
//!
//! The mapping into the exclusive buckets (ARCHITECTURE §4.1.3, RESEARCH_PLAN
//! §5.13): `input_tokens` already excludes cache reads; `output_tokens` is
//! `thinking_output_tokens + response_output_tokens`, so `output` is the
//! response part and `thinking` the reasoning part. A record whose parts do
//! not add up is clamped against its total and reported as drift.

use std::collections::btree_map::Entry;

use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{LineOutcome, LineStatus, NormalizeCtx};
use rosetta_core::rows::{DriftFact, Ms, ServiceTier, SessionUsageRow, TokenSnapshot, UsageScope};
use rosetta_core::{IngestOp, ids};
use serde_json::Value;

use crate::normalize::AntigravityState;
use crate::raw::conversation::{
    GEN_METADATA_QUERY, GenMetadataRow, ParsedRow, STEPS_QUERY, SnapshotRecord, StepRow, parse_row,
};
use crate::raw::cortex::{GeneratorMetadata, ModelUsageStats, StepMetadata};

const UNKNOWN_QUERY: &str = "unknown_query";
const UNKNOWN_FIELDS: &str = "unknown_fields";
const PARSE_FAILED: &str = "parse_failed";
const INVALID_JSON: &str = "invalid_json";
const INVALID_UTF8: &str = "invalid_utf8";
const UNKNOWN_SERVICE_TIER: &str = "unknown_service_tier";
const USAGE_SUBSET_EXCEEDS_TOTAL: &str = "usage_subset_exceeds_total";
const GENERATION_STEP_CONFLICT: &str = "generation_step_conflict";
const INVALID_RECORD: &str = "<invalid-json>";
const NO_QUERY: &str = "<untyped>";

/// Prefix of the native replacement key; distinct from every transcript
/// discriminator so the two streams can never collide inside one session.
const KEY_PREFIX: &str = "conversation-db:step#";

pub fn ingest(
    ctx: &mut NormalizeCtx,
    state: &mut AntigravityState,
    stream_id: &str,
    line: &[u8],
) -> LineOutcome {
    let Ok(text) = std::str::from_utf8(line) else {
        return hard(ctx, stream_id, INVALID_RECORD, INVALID_UTF8, &[]);
    };
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return hard(ctx, stream_id, INVALID_RECORD, INVALID_JSON, &[]);
    };
    let record: SnapshotRecord = match serde_json::from_value(value.clone()) {
        Ok(record) => record,
        Err(_) => {
            let kind = value["query"].as_str().unwrap_or(NO_QUERY);
            return hard(ctx, stream_id, kind, PARSE_FAILED, &keys(&value));
        }
    };

    let mut emit = Emit::new(ctx, stream_id, state);
    if !record.unknown.is_empty() {
        let paths: Vec<String> = record.unknown.keys().map(|key| format!(".{key}")).collect();
        emit.drift(&record.query, UNKNOWN_FIELDS, &paths, None);
    }

    let parsed = match parse_row(&record.query, record.row.clone()) {
        Ok(parsed) => parsed,
        Err(_) => {
            emit.drift(&record.query, PARSE_FAILED, &keys(&record.row), None);
            return emit.finish(LineStatus::HardError {
                code: PARSE_FAILED.to_owned(),
            });
        }
    };
    let status = match parsed {
        ParsedRow::UnknownQuery { query, row } => {
            emit.drift(&query, UNKNOWN_QUERY, &keys(&row), None);
            LineStatus::UnknownType
        }
        ParsedRow::GenMetadata(row) => generation(&mut emit, &row),
        ParsedRow::Step(row) => step(&mut emit, &row),
    };
    emit.finish(status)
}

/// Index the generation's steps by the model it served. The records are
/// ordered so every generation precedes every step, and the whole database
/// replays through one normalizer context, so the index is complete before
/// the first step asks it.
fn generation(emit: &mut Emit<'_>, row: &GenMetadataRow) -> LineStatus {
    let paths = row.drift_paths();
    if !paths.is_empty() {
        emit.drift(GEN_METADATA_QUERY, UNKNOWN_FIELDS, &paths, None);
    }
    let Some(blob) = &row.data else {
        return LineStatus::Parsed;
    };
    let decoded = blob
        .bytes()
        .and_then(|bytes| GeneratorMetadata::decode(&bytes).ok());
    let Some(generation) = decoded else {
        emit.drift(
            GEN_METADATA_QUERY,
            PARSE_FAILED,
            &[".data".to_owned()],
            None,
        );
        return LineStatus::HardError {
            code: PARSE_FAILED.to_owned(),
        };
    };
    let paths = generation.drift_paths();
    if !paths.is_empty() {
        let paths: Vec<String> = paths.iter().map(|path| format!(".data{path}")).collect();
        emit.drift(GEN_METADATA_QUERY, UNKNOWN_FIELDS, &paths, None);
    }

    let model = generation
        .chat_model
        .as_ref()
        .and_then(|chat| chat.response_model.clone());
    for index in &generation.step_indices {
        match emit.state.generations.entry(*index) {
            Entry::Vacant(slot) => {
                slot.insert(model.clone());
            }
            // Two generations naming one step would double-attribute it; the
            // first claim stands and the second is reported.
            Entry::Occupied(_) => emit.drift(
                GEN_METADATA_QUERY,
                GENERATION_STEP_CONFLICT,
                &[format!(".data.step_indices[{index}]")],
                None,
            ),
        }
    }
    LineStatus::Parsed
}

fn step(emit: &mut Emit<'_>, row: &StepRow) -> LineStatus {
    let paths = row.drift_paths();
    if !paths.is_empty() {
        emit.drift(STEPS_QUERY, UNKNOWN_FIELDS, &paths, None);
    }
    let Some(blob) = &row.metadata else {
        return LineStatus::Parsed;
    };
    let decoded = blob
        .bytes()
        .and_then(|bytes| StepMetadata::decode(&bytes).ok());
    let Some(metadata) = decoded else {
        emit.drift(STEPS_QUERY, PARSE_FAILED, &[".metadata".to_owned()], None);
        return LineStatus::HardError {
            code: PARSE_FAILED.to_owned(),
        };
    };
    let at = metadata
        .created_at
        .as_ref()
        .and_then(|stamp| stamp.epoch_ms());
    let paths = metadata.drift_paths();
    if !paths.is_empty() {
        let paths: Vec<String> = paths
            .iter()
            .map(|path| format!(".metadata{path}"))
            .collect();
        emit.drift(STEPS_QUERY, UNKNOWN_FIELDS, &paths, at);
    }

    let Some(usage) = metadata
        .model_usage
        .as_ref()
        .filter(|usage| !usage.is_empty())
    else {
        return LineStatus::Parsed;
    };
    let (output, thinking, malformed) = output_split(usage);
    if malformed {
        emit.drift(
            STEPS_QUERY,
            USAGE_SUBSET_EXCEEDS_TOTAL,
            &[
                ".metadata.model_usage.thinking_output_tokens".to_owned(),
                ".metadata.model_usage.response_output_tokens".to_owned(),
            ],
            at,
        );
    }
    let service_tier = match usage.service_tier.as_deref() {
        None => None,
        Some(raw) => {
            let tier = service_tier(raw);
            if tier.is_none() {
                emit.drift(
                    STEPS_QUERY,
                    UNKNOWN_SERVICE_TIER,
                    &[".metadata.model_usage.service_tier".to_owned()],
                    at,
                );
            }
            tier
        }
    };
    // A writer that folds several attempts into one step lists them in
    // `retry_infos`; the generation-level record proves that its usage is
    // the sum of its attempts, so such a step is a counted aggregate.
    let attempts = metadata.retry_infos.len();
    let (scope, requests) = if attempts >= 2 {
        (UsageScope::Aggregate, Some(attempts as i64))
    } else {
        (UsageScope::Request, None)
    };
    let model = u32::try_from(row.idx)
        .ok()
        .and_then(|index| emit.state.generations.get(&index).cloned())
        .flatten();

    let session_id = emit.ctx.session_id.clone();
    emit.ops.push(IngestOp::SessionUsage(SessionUsageRow {
        identity: None,
        key: ids::usage_id(&session_id, &format!("{KEY_PREFIX}{}", row.idx)),
        session_id,
        // No timestamp means "never priced as 1970": the store treats 0 as
        // unknown.
        ts: at.unwrap_or(0),
        model,
        tokens: TokenSnapshot {
            input: Some(clamp(usage.input_tokens)),
            output: Some(clamp(output)),
            cache_read: Some(clamp(usage.cache_read_tokens)),
            cache_write: Some(clamp(usage.cache_write_tokens)),
            thinking: thinking.map(clamp),
            service_tier,
            scope,
            requests,
            ..TokenSnapshot::default()
        },
    }));
    LineStatus::Parsed
}

/// Split `output_tokens` into the exclusive `output` and `thinking` buckets.
///
/// Every observed record satisfies `output_tokens = thinking + response`
/// (47 of 47 organic requests, every fixture request). A record with neither
/// part is a writer without the split: its whole output is response and its
/// reasoning is unreported rather than zero. A record whose parts disagree
/// with the total is malformed: the total wins, thinking is clamped to it,
/// and the remainder is output.
fn output_split(usage: &ModelUsageStats) -> (u64, Option<u64>, bool) {
    let total = usage.output_tokens;
    let thinking = usage.thinking_output_tokens;
    let response = usage.response_output_tokens;
    if total == 0 || total == thinking.saturating_add(response) {
        return (response, Some(thinking), false);
    }
    if thinking == 0 && response == 0 {
        return (total, None, false);
    }
    let thinking = thinking.min(total);
    (total - thinking, Some(thinking), true)
}

/// The closed tier vocabulary. `ModelUsageStats.service_tier` is a free
/// provider string (Antigravity fronts Google, Anthropic and OpenAI
/// backends), so the spellings each of them documents are accepted and
/// anything else is drift.
fn service_tier(raw: &str) -> Option<ServiceTier> {
    Some(match raw.trim().to_ascii_lowercase().as_str() {
        "standard" | "default" | "auto" | "standard_only" => ServiceTier::Standard,
        "priority" | "scale" => ServiceTier::Priority,
        "flex" => ServiceTier::Flex,
        "batch" => ServiceTier::Batch,
        "fast" => ServiceTier::Fast,
        _ => return None,
    })
}

fn clamp(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn keys(value: &Value) -> Vec<String> {
    match value.as_object() {
        Some(object) => object.keys().map(|key| format!(".{key}")).collect(),
        None => vec!["<non-object>".to_owned()],
    }
}

struct Emit<'a> {
    ctx: &'a NormalizeCtx,
    stream_id: &'a str,
    state: &'a mut AntigravityState,
    ops: Vec<IngestOp>,
}

impl<'a> Emit<'a> {
    fn new(ctx: &'a NormalizeCtx, stream_id: &'a str, state: &'a mut AntigravityState) -> Self {
        Self {
            ctx,
            stream_id,
            state,
            ops: Vec::new(),
        }
    }

    fn drift(&mut self, record_kind: &str, code: &str, paths: &[String], at: Option<Ms>) {
        self.ops.push(drift(
            self.ctx,
            self.stream_id,
            record_kind,
            code,
            paths,
            at,
        ));
    }

    fn finish(self, status: LineStatus) -> LineOutcome {
        LineOutcome {
            ops: self.ops,
            status,
        }
    }
}

fn drift(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    code: &str,
    paths: &[String],
    at: Option<Ms>,
) -> IngestOp {
    IngestOp::Drift(DriftFact {
        runtime_id: ctx.runtime_id.clone(),
        agent_version: ctx.agent_version.clone().unwrap_or_default(),
        stream: stream_id.to_owned(),
        record_kind: record_kind.to_owned(),
        error_code: code.to_owned(),
        shape_hash: ids::shape_hash(paths),
        at,
        sample_cursor: ctx.src_cursor,
        sample_offset: ctx.src_offset,
    })
}

fn hard(
    ctx: &NormalizeCtx,
    stream_id: &str,
    record_kind: &str,
    code: &str,
    paths: &[String],
) -> LineOutcome {
    LineOutcome {
        ops: vec![drift(ctx, stream_id, record_kind, code, paths, None)],
        status: LineStatus::HardError {
            code: code.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{output_split, service_tier};
    use crate::AntigravityAdapter;
    use crate::normalize::AntigravityState;
    use crate::raw::base64;
    use crate::raw::cortex::ModelUsageStats;
    use crate::raw::wire::encode::{field_bytes, field_str, field_varint, message, packed_varints};
    use crate::spec;
    use rosetta_adapter_support::adapter::{AgentAdapter, LineOutcome, LineStatus, NormalizeCtx};
    use rosetta_core::rows::{ServiceTier, UsageScope};
    use rosetta_core::{IngestOp, ids};
    use serde_json::{Value, json};

    const CASCADE: &str = "11111111-2222-3333-4444-555555555555";

    fn context() -> NormalizeCtx {
        let runtime = ids::runtime_id("antigravity", "~/.gemini/antigravity-cli");
        let session = ids::session_id(&runtime, CASCADE);
        let mut ctx = NormalizeCtx::new(runtime, session);
        ctx.native_session_id = Some(CASCADE.to_owned());
        ctx.state = AntigravityAdapter.init_state();
        ctx
    }

    fn ingest(ctx: &mut NormalizeCtx, value: Value) -> LineOutcome {
        AntigravityAdapter.ingest_line(
            ctx,
            spec::CONVERSATION_DB,
            &serde_json::to_vec(&value).unwrap(),
        )
    }

    fn blob(bytes: &[u8]) -> Value {
        json!({"$base64": base64::encode(bytes)})
    }

    fn usage(input: u64, thinking: u64, response: u64, cache_read: u64) -> Vec<u8> {
        message(&[
            field_varint(1, 1298),
            field_varint(2, input),
            field_varint(3, thinking + response),
            field_varint(5, cache_read),
            field_varint(6, 24),
            field_varint(9, thinking),
            field_varint(10, response),
        ])
    }

    fn stamp(seconds: u64, nanos: u64) -> Vec<u8> {
        message(&[field_varint(1, seconds), field_varint(2, nanos)])
    }

    fn generation_record(idx: i64, steps: &[u64], model: Option<&str>) -> Value {
        let mut chat = vec![field_varint(3, 1298), field_bytes(4, &usage(128, 0, 32, 0))];
        if let Some(model) = model {
            chat.push(field_str(19, model));
        }
        let data = message(&[field_bytes(1, &message(&chat)), packed_varints(2, steps)]);
        json!({"query": "gen_metadata", "row": {"idx": idx, "data": blob(&data)}})
    }

    fn step_record(idx: i64, step_type: i64, metadata: &[u8]) -> Value {
        json!({
            "query": "steps",
            "row": {"idx": idx, "step_type": step_type, "status": 3, "metadata": blob(metadata)}
        })
    }

    fn planner_step(idx: i64, usage_bytes: &[u8]) -> Value {
        let metadata = message(&[
            field_bytes(1, &stamp(1_787_634_643, 907_000_000)),
            field_varint(3, 2),
            field_bytes(9, usage_bytes),
            field_varint(11, 1298),
        ]);
        step_record(idx, 15, &metadata)
    }

    fn samples(outcome: &LineOutcome) -> Vec<&rosetta_core::rows::SessionUsageRow> {
        outcome
            .ops
            .iter()
            .filter_map(|op| match op {
                IngestOp::SessionUsage(row) => Some(row),
                _ => None,
            })
            .collect()
    }

    fn drift_codes(outcome: &LineOutcome) -> Vec<String> {
        outcome
            .ops
            .iter()
            .filter_map(|op| match op {
                IngestOp::Drift(fact) => Some(fact.error_code.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_step_takes_its_model_from_the_generation_that_owns_it() {
        let mut ctx = context();
        let generation = ingest(
            &mut ctx,
            generation_record(0, &[1, 2], Some("gemini-3.7-flash")),
        );
        assert_eq!(generation.status, LineStatus::Parsed);
        assert!(generation.ops.is_empty());

        let outcome = ingest(&mut ctx, planner_step(1, &usage(14_181, 145, 51, 16_260)));
        assert_eq!(outcome.status, LineStatus::Parsed);
        let [sample] = samples(&outcome)[..] else {
            panic!("expected one usage sample, got {:?}", outcome.ops);
        };
        assert_eq!(sample.model.as_deref(), Some("gemini-3.7-flash"));
        assert_eq!(sample.ts, 1_787_634_643_907);
        assert_eq!(sample.session_id, ctx.session_id);
        assert_eq!(
            sample.key,
            ids::usage_id(&ctx.session_id, "conversation-db:step#1")
        );
        let tokens = &sample.tokens;
        assert_eq!(tokens.input, Some(14_181));
        assert_eq!(tokens.cache_read, Some(16_260));
        assert_eq!(tokens.cache_write, Some(0));
        assert_eq!(tokens.output, Some(51));
        assert_eq!(tokens.thinking, Some(145));
        assert_eq!(tokens.total_tokens(), Some(14_181 + 16_260 + 51 + 145));
        assert_eq!(tokens.scope, UsageScope::Request);
        assert_eq!(tokens.requests, None);
        assert_eq!(tokens.service_tier, None);
        assert_eq!(tokens.route, None);
        assert_eq!(tokens.billed_pico_usd, None);
        assert!(drift_codes(&outcome).is_empty());
    }

    #[test]
    fn unowned_and_placeholder_only_steps_stay_unpriced() {
        let mut ctx = context();
        // A generation whose backend never reported a model string.
        let _ = ingest(&mut ctx, generation_record(0, &[1], None));
        let placeholder = ingest(&mut ctx, planner_step(1, &usage(128, 0, 32, 0)));
        assert_eq!(samples(&placeholder)[0].model, None);

        // The checkpoint summarizer's call: no generation names it.
        let checkpoint = message(&[
            field_bytes(1, &stamp(1_787_634_643, 950_000_000)),
            field_varint(3, 5),
            field_bytes(9, &usage(200, 0, 27, 0)),
            field_bytes(28, &message(&[field_bytes(2, &usage(200, 0, 27, 0))])),
        ]);
        let orphan = ingest(&mut ctx, step_record(2, 23, &checkpoint));
        let [sample] = samples(&orphan)[..] else {
            panic!("expected one usage sample, got {:?}", orphan.ops);
        };
        assert_eq!(sample.model, None);
        assert_eq!(sample.tokens.total_tokens(), Some(227));
        assert_eq!(sample.tokens.scope, UsageScope::Request, "one attempt");
        assert!(drift_codes(&orphan).is_empty());
    }

    #[test]
    fn steps_without_usage_emit_nothing() {
        let mut ctx = context();
        let user_input = message(&[field_bytes(1, &stamp(1, 0)), field_varint(3, 4)]);
        assert!(
            ingest(&mut ctx, step_record(0, 14, &user_input))
                .ops
                .is_empty()
        );
        let empty_usage = message(&[field_bytes(9, &message(&[field_varint(1, 1298)]))]);
        assert!(
            ingest(&mut ctx, step_record(1, 15, &empty_usage))
                .ops
                .is_empty()
        );
        let no_metadata = json!({"query": "steps", "row": {"idx": 2, "step_type": 21, "status": 3, "metadata": null}});
        let outcome = ingest(&mut ctx, no_metadata);
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert!(outcome.ops.is_empty());
    }

    #[test]
    fn several_attempts_in_one_step_are_a_counted_aggregate() {
        let mut ctx = context();
        let attempt = usage(100, 0, 10, 0);
        let metadata = message(&[
            field_bytes(1, &stamp(1, 0)),
            field_bytes(9, &usage(200, 0, 20, 0)),
            field_bytes(28, &message(&[field_bytes(2, &attempt)])),
            field_bytes(
                28,
                &message(&[field_bytes(2, &attempt), field_varint(5, 1)]),
            ),
        ]);
        let outcome = ingest(&mut ctx, step_record(3, 15, &metadata));
        let sample = samples(&outcome)[0];
        assert_eq!(sample.tokens.scope, UsageScope::Aggregate);
        assert_eq!(sample.tokens.requests, Some(2));
        assert_eq!(sample.tokens.input, Some(200));
    }

    #[test]
    fn a_missing_timestamp_is_zero_never_1970() {
        let mut ctx = context();
        let metadata = message(&[field_bytes(9, &usage(1, 0, 1, 0))]);
        let outcome = ingest(&mut ctx, step_record(4, 15, &metadata));
        assert_eq!(samples(&outcome)[0].ts, 0);
    }

    #[test]
    fn service_tier_maps_known_spellings_and_reports_the_rest() {
        assert_eq!(service_tier("priority"), Some(ServiceTier::Priority));
        assert_eq!(service_tier("Default"), Some(ServiceTier::Standard));
        assert_eq!(service_tier("flex"), Some(ServiceTier::Flex));
        assert_eq!(service_tier("batch"), Some(ServiceTier::Batch));
        assert_eq!(service_tier("fast"), Some(ServiceTier::Fast));
        assert_eq!(service_tier("turbo"), None);

        let mut ctx = context();
        let tiered = |tier: &str| {
            let mut fields = usage(10, 0, 5, 0);
            fields.extend(field_str(13, tier));
            message(&[field_bytes(9, &fields)])
        };
        let known = ingest(&mut ctx, step_record(1, 15, &tiered("priority")));
        assert_eq!(
            samples(&known)[0].tokens.service_tier,
            Some(ServiceTier::Priority)
        );
        assert!(drift_codes(&known).is_empty());
        let unknown = ingest(&mut ctx, step_record(2, 15, &tiered("turbo")));
        assert_eq!(unknown.status, LineStatus::Parsed);
        assert_eq!(samples(&unknown)[0].tokens.service_tier, None);
        assert_eq!(drift_codes(&unknown), ["unknown_service_tier"]);
    }

    #[test]
    fn output_split_follows_the_proven_identity_and_clamps_the_rest() {
        let record = |total, thinking, response| ModelUsageStats {
            output_tokens: total,
            thinking_output_tokens: thinking,
            response_output_tokens: response,
            ..ModelUsageStats::default()
        };
        assert_eq!(output_split(&record(196, 145, 51)), (51, Some(145), false));
        assert_eq!(output_split(&record(32, 0, 32)), (32, Some(0), false));
        assert_eq!(output_split(&record(0, 0, 0)), (0, Some(0), false));
        // A writer without the split: reasoning is unreported, not zero.
        assert_eq!(output_split(&record(40, 0, 0)), (40, None, false));
        // Parts without a total: the parts are exclusive by definition.
        assert_eq!(output_split(&record(0, 5, 7)), (7, Some(5), false));
        // Parts that exceed the total: the total wins, thinking is clamped.
        assert_eq!(output_split(&record(100, 80, 50)), (20, Some(80), true));
        assert_eq!(output_split(&record(10, 30, 0)), (0, Some(10), true));

        let mut ctx = context();
        let malformed = message(&[
            field_varint(2, 1),
            field_varint(3, 100),
            field_varint(9, 80),
            field_varint(10, 50),
        ]);
        let outcome = ingest(
            &mut ctx,
            step_record(1, 15, &message(&[field_bytes(9, &malformed)])),
        );
        assert_eq!(drift_codes(&outcome), ["usage_subset_exceeds_total"]);
        let tokens = &samples(&outcome)[0].tokens;
        assert_eq!((tokens.output, tokens.thinking), (Some(20), Some(80)));
    }

    #[test]
    fn a_second_generation_claiming_a_step_is_drift_and_the_first_wins() {
        let mut ctx = context();
        let _ = ingest(
            &mut ctx,
            generation_record(0, &[1], Some("gemini-3.7-flash")),
        );
        let conflict = ingest(
            &mut ctx,
            generation_record(1, &[1, 3], Some("gemini-3.6-flash")),
        );
        assert_eq!(conflict.status, LineStatus::Parsed);
        assert_eq!(drift_codes(&conflict), ["generation_step_conflict"]);
        let first = ingest(&mut ctx, planner_step(1, &usage(1, 0, 1, 0)));
        assert_eq!(
            samples(&first)[0].model.as_deref(),
            Some("gemini-3.7-flash")
        );
        let third = ingest(&mut ctx, planner_step(3, &usage(1, 0, 1, 0)));
        assert_eq!(
            samples(&third)[0].model.as_deref(),
            Some("gemini-3.6-flash")
        );
    }

    #[test]
    fn undeclared_protobuf_fields_and_columns_are_soft_drift() {
        let mut ctx = context();
        let metadata = message(&[
            field_bytes(
                9,
                &message(&[field_varint(2, 3), field_varint(10, 1), field_varint(99, 7)]),
            ),
            field_str(40, "future"),
        ]);
        let mut record = step_record(1, 15, &metadata);
        record["row"]["later_column"] = json!(true);
        let outcome = ingest(&mut ctx, record);
        assert_eq!(outcome.status, LineStatus::Parsed);
        assert_eq!(drift_codes(&outcome), ["unknown_fields", "unknown_fields"]);
        assert_eq!(samples(&outcome).len(), 1);

        let mut generation = generation_record(0, &[1], None);
        generation["extra"] = json!(1);
        let outcome = ingest(&mut ctx, generation);
        assert_eq!(drift_codes(&outcome), ["unknown_fields"]);
    }

    #[test]
    fn unframeable_blobs_are_hard_errors() {
        let mut ctx = context();
        let bad_base64 = json!({"query": "steps", "row": {"idx": 1, "step_type": 15, "status": 3, "metadata": {"$base64": "not base64"}}});
        let outcome = ingest(&mut ctx, bad_base64);
        assert_eq!(
            outcome.status,
            LineStatus::HardError {
                code: "parse_failed".to_owned()
            }
        );
        assert_eq!(drift_codes(&outcome), ["parse_failed"]);

        let truncated =
            json!({"query": "gen_metadata", "row": {"idx": 0, "data": blob(&[0x0A, 0x05, 0x01])}});
        let outcome = ingest(&mut ctx, truncated);
        assert!(matches!(outcome.status, LineStatus::HardError { .. }));

        let moved_column = json!({"query": "steps", "row": {"idx": "one"}});
        let outcome = ingest(&mut ctx, moved_column);
        assert!(matches!(outcome.status, LineStatus::HardError { .. }));

        let outcome = AntigravityAdapter.ingest_line(&mut ctx, spec::CONVERSATION_DB, b"{not json");
        assert_eq!(
            outcome.status,
            LineStatus::HardError {
                code: "invalid_json".to_owned()
            }
        );
    }

    #[test]
    fn unknown_queries_are_soft_and_other_streams_are_ignored() {
        let mut ctx = context();
        let outcome = ingest(
            &mut ctx,
            json!({"query": "executor_metadata", "row": {"idx": 0}}),
        );
        assert_eq!(outcome.status, LineStatus::UnknownType);
        assert_eq!(drift_codes(&outcome), ["unknown_query"]);

        let record = planner_step(1, &usage(1, 0, 1, 0));
        let ignored = AntigravityAdapter.ingest_line(
            &mut ctx,
            spec::CONVERSATION_SUMMARIES,
            &serde_json::to_vec(&record).unwrap(),
        );
        assert!(ignored.ops.is_empty());
    }

    #[test]
    fn the_generation_index_survives_a_checkpoint_round_trip() {
        let adapter = AntigravityAdapter;
        let mut ctx = context();
        let _ = ingest(
            &mut ctx,
            generation_record(0, &[1, 2], Some("gemini-3.7-flash")),
        );
        let _ = ingest(&mut ctx, generation_record(1, &[4], None));
        let checkpoint = adapter
            .checkpoint_state(ctx.state.as_deref())
            .unwrap()
            .expect("checkpoint");
        assert_eq!(
            checkpoint,
            json!({
                "pending_tools": [],
                "generations": {"1": "gemini-3.7-flash", "2": "gemini-3.7-flash", "4": null}
            })
        );
        let restored = adapter
            .restore_state(Some(&checkpoint))
            .unwrap()
            .expect("state");
        assert_eq!(
            restored.downcast_ref::<AntigravityState>(),
            ctx.state
                .as_deref()
                .and_then(|state| state.downcast_ref::<AntigravityState>())
        );
    }
}
