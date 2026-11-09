//! Fixture replay and golden normalization of the `conversation-db` usage
//! stream. Every committed conversation store must frame through the declared
//! plan, decode with zero drift, and yield exactly the usage the audit counted
//! by hand: one sample per step that carries `model_usage`, summing to each
//! generation's own total.
//!
//! Regenerate the goldens with:
//! `UPDATE_GOLDEN=1 cargo test -p rosetta-adapter-antigravity --test conversation_db`.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rosetta_adapter_antigravity::raw::conversation::{ParsedRow, SnapshotRecord, parse_row};
use rosetta_adapter_antigravity::raw::cortex::{GeneratorMetadata, StepMetadata};
use rosetta_adapter_antigravity::{AntigravityAdapter, spec};
use rosetta_adapter_support::Drift;
use rosetta_adapter_support::adapter::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::rows::{SessionUsageRow, UsageScope};
use rosetta_core::{IngestOp, ids};

use support::{DATA_ROOT, FramedStore, conversation_dbs, frame};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn framed(db: &Path) -> FramedStore {
    let plan = AntigravityAdapter
        .snapshot_plan(spec::CONVERSATION_DB)
        .expect("conversation store plan");
    frame(db, &plan)
}

fn normalize(store: &FramedStore) -> (Vec<IngestOp>, Vec<String>) {
    let runtime = ids::runtime_id("antigravity", DATA_ROOT);
    let session = ids::session_id(&runtime, &store.native_session_id);
    let mut ctx = NormalizeCtx::new(runtime, session);
    ctx.native_session_id = Some(store.native_session_id.clone());
    ctx.state = AntigravityAdapter.init_state();
    ctx.src_cursor = Some(1);

    let mut ops = Vec::new();
    let mut unhealthy = Vec::new();
    for (index, (query, record)) in store.records.iter().enumerate() {
        ctx.src_offset = Some(index as i64);
        let outcome = AntigravityAdapter.ingest_line(&mut ctx, spec::CONVERSATION_DB, record);
        if outcome.status != LineStatus::Parsed {
            unhealthy.push(format!("{query} record {index}: {:?}", outcome.status));
        }
        ops.extend(outcome.ops);
    }
    (ops, unhealthy)
}

fn samples(ops: &[IngestOp]) -> Vec<&SessionUsageRow> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::SessionUsage(row) => Some(row),
            _ => None,
        })
        .collect()
}

#[test]
fn corpus_exists() {
    assert!(
        !conversation_dbs().is_empty(),
        "no conversation stores under {} — run: python3 -m tools.groundtruth farm antigravity",
        support::fixtures_root().display()
    );
}

/// The store's cascade id IS the brain conversation UUID, so the usage
/// stream lands on the very session the transcript stream creates.
#[test]
fn every_store_names_the_transcript_conversation() {
    for (version, scenario, db) in conversation_dbs() {
        let store = framed(&db);
        let stem = db.file_stem().unwrap().to_string_lossy().into_owned();
        assert_eq!(store.native_session_id, stem, "{version}/{scenario}");
        let brain = db
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("brain")
            .join(&stem)
            .join(".system_generated/logs/transcript.jsonl");
        assert!(
            brain.is_file(),
            "{version}/{scenario}: no transcript for cascade {stem}"
        );
        let queries: Vec<&str> = store
            .records
            .iter()
            .map(|(query, _)| query.as_str())
            .collect();
        let first_step = queries.iter().position(|query| *query == "steps");
        let last_generation = queries.iter().rposition(|query| *query == "gen_metadata");
        assert!(
            last_generation < first_step,
            "{version}/{scenario}: generations must precede steps"
        );
    }
}

#[test]
fn every_row_decodes_without_drift() {
    let mut rows = 0_usize;
    let mut drift = Vec::new();
    for (version, scenario, db) in conversation_dbs() {
        for (index, (_, record)) in framed(&db).records.iter().enumerate() {
            rows += 1;
            let envelope: SnapshotRecord = serde_json::from_slice(record).unwrap();
            assert!(envelope.unknown.is_empty());
            let parsed = parse_row(&envelope.query, envelope.row).unwrap();
            let paths = match parsed {
                ParsedRow::GenMetadata(row) => {
                    let mut paths = row.drift_paths();
                    let bytes = row.data.as_ref().expect("generation blob").bytes().unwrap();
                    let generation = GeneratorMetadata::decode(&bytes).unwrap();
                    assert!(
                        generation.chat_model.is_some(),
                        "{version}/{scenario} {index}"
                    );
                    paths.extend(generation.drift_paths());
                    paths
                }
                ParsedRow::Step(row) => {
                    let mut paths = row.drift_paths();
                    let bytes = row.metadata.as_ref().expect("step blob").bytes().unwrap();
                    let step = StepMetadata::decode(&bytes).unwrap();
                    assert!(step.created_at.is_some(), "{version}/{scenario} {index}");
                    paths.extend(step.drift_paths());
                    paths
                }
                ParsedRow::UnknownQuery { query, .. } => {
                    panic!("{version}/{scenario}: undeclared query {query}")
                }
            };
            if !paths.is_empty() {
                drift.push(format!(
                    "{version}/{scenario} record {index}: {}",
                    paths.join(", ")
                ));
            }
        }
    }
    assert!(rows > 0, "corpus framed no rows");
    assert!(
        drift.is_empty(),
        "undeclared protobuf fields or columns in committed corpus (extend the ground truth):\n{}",
        drift.join("\n")
    );
}

/// One expected usage sample: the step it is keyed by, its own date, and its
/// `(input, output, thinking, cache_read)` tokens.
type Sample = (u32, i64, (i64, i64, i64, i64));
/// A store's expected served model and samples, keyed by `(version, scenario)`.
type HandCount = BTreeMap<(&'static str, &'static str), (Option<&'static str>, Vec<Sample>)>;

/// The audit's hand count, per writer version.
///
/// 1.1.20: five planner steps and three checkpoint steps carry usage, every
/// one 128 in / 32 out from the mock, with no model string (the mock reported
/// none) and therefore unpriced.
///
/// 1.4.12: no checkpoint steps; every planner step carries usage and every
/// generation names the served model `gemini-3.1-pro-preview`. The `usage`
/// scenario's mock reports distinct non-zero cache reads and thinking tokens.
///
/// Nothing else in the stores is usage.
#[test]
fn usage_samples_match_the_fixture_evidence() {
    const MOCK: (i64, i64, i64, i64) = (128, 32, 0, 0);
    let expected: HandCount = BTreeMap::from([
        (
            ("1.1.20", "basic-chat"),
            (
                None,
                vec![(1, 1_787_634_643_900, MOCK), (2, 1_787_634_643_907, MOCK)],
            ),
        ),
        (
            ("1.1.20", "resume"),
            (
                None,
                vec![
                    (1, 1_787_634_645_416, MOCK),
                    (2, 1_787_634_645_422, MOCK),
                    (5, 1_787_634_646_027, MOCK),
                ],
            ),
        ),
        (
            ("1.1.20", "tool-use"),
            (
                None,
                vec![
                    (1, 1_787_634_644_612, MOCK),
                    (3, 1_787_634_644_627, MOCK),
                    (4, 1_787_634_644_629, MOCK),
                ],
            ),
        ),
        (
            ("1.4.12", "basic-chat"),
            (Some(SERVED_1_4_12), vec![(1, 1_793_818_671_124, MOCK)]),
        ),
        (
            ("1.4.12", "resume"),
            (
                Some(SERVED_1_4_12),
                vec![(1, 1_793_818_672_365, MOCK), (4, 1_793_818_672_868, MOCK)],
            ),
        ),
        (
            ("1.4.12", "tool-use"),
            (
                Some(SERVED_1_4_12),
                vec![(1, 1_793_818_671_755, MOCK), (3, 1_793_818_671_785, MOCK)],
            ),
        ),
        (
            ("1.4.12", "usage"),
            (
                Some(SERVED_1_4_12),
                vec![
                    (1, 1_793_818_673_452, (1009, 197, 211, 5003)),
                    (3, 1_793_818_673_487, (1109, 207, 221, 6003)),
                ],
            ),
        ),
    ]);
    let mut seen = 0;
    for (version, scenario, db) in conversation_dbs() {
        let store = framed(&db);
        let (ops, unhealthy) = normalize(&store);
        assert!(unhealthy.is_empty(), "{version}/{scenario}: {unhealthy:?}");
        let session = ids::session_id(
            &ids::runtime_id("antigravity", DATA_ROOT),
            &store.native_session_id,
        );
        let samples = samples(&ops);
        let (model, want) = expected
            .get(&(version.as_str(), scenario.as_str()))
            .unwrap_or_else(|| panic!("{version}/{scenario}: no hand count"));
        assert_eq!(samples.len(), want.len(), "{version}/{scenario}");
        seen += 1;
        for (sample, (step, ts, (input, output, thinking, cache_read))) in samples.iter().zip(want)
        {
            assert_eq!(sample.session_id, session);
            assert_eq!(
                sample.key,
                ids::usage_id(&session, &format!("conversation-db:step#{step}")),
                "{version}/{scenario} step {step}"
            );
            assert_eq!(sample.ts, *ts, "{version}/{scenario} step {step}");
            assert_eq!(
                sample.model.as_deref(),
                *model,
                "{version}/{scenario} step {step}"
            );
            let tokens = &sample.tokens;
            assert_eq!(
                tokens.input,
                Some(*input),
                "{version}/{scenario} step {step}"
            );
            assert_eq!(
                tokens.output,
                Some(*output),
                "{version}/{scenario} step {step}"
            );
            assert_eq!(
                tokens.thinking,
                Some(*thinking),
                "{version}/{scenario} step {step}"
            );
            assert_eq!(
                tokens.cache_read,
                Some(*cache_read),
                "{version}/{scenario} step {step}"
            );
            assert_eq!(tokens.cache_write, Some(0));
            assert_eq!(tokens.cache_write_1h, None);
            assert_eq!(tokens.service_tier, None);
            assert_eq!(tokens.scope, UsageScope::Request);
            assert_eq!(tokens.requests, None);
        }
        assert!(
            ops.iter().all(|op| matches!(op, IngestOp::SessionUsage(_))),
            "{version}/{scenario}: the stream emits usage samples only, got {ops:?}"
        );
    }
    assert_eq!(
        seen,
        expected.len(),
        "every hand-counted store is committed"
    );
}

const SERVED_1_4_12: &str = "gemini-3.1-pro-preview";

/// 1.4.12 also writes each request's tokens on its transcript planner step.
/// They are the very request the store's step sample describes — same step
/// index, `input_tokens` and `cache_read_tokens` equal, transcript
/// `output_tokens` = thinking + response — which is why the transcript copy
/// stays unprojected: the store is the one usage source.
#[test]
fn writer_1_4_12_planner_tokens_restate_the_store_samples() {
    let mut compared = 0;
    for (version, scenario, db) in conversation_dbs() {
        let store = framed(&db);
        let transcript = db
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("brain")
            .join(&store.native_session_id)
            .join(".system_generated/logs/transcript.jsonl");
        let mut planner = BTreeMap::new();
        for line in std::fs::read_to_string(&transcript).unwrap().lines() {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            if record["type"] == "PLANNER_RESPONSE" && record.get("input_tokens").is_some() {
                planner.insert(record["step_index"].as_u64().unwrap(), record);
            }
        }
        if version == "1.1.20" {
            assert!(
                planner.is_empty(),
                "{version}/{scenario}: 1.1.20 planner steps carry no tokens"
            );
            continue;
        }
        let (ops, _) = normalize(&store);
        let samples = samples(&ops);
        assert_eq!(samples.len(), planner.len(), "{version}/{scenario}");
        let session = &samples[0].session_id;
        for (step, record) in &planner {
            let sample = samples
                .iter()
                .find(|sample| {
                    sample.key == ids::usage_id(session, &format!("conversation-db:step#{step}"))
                })
                .unwrap_or_else(|| panic!("{version}/{scenario}: no store sample for step {step}"));
            let tokens = &sample.tokens;
            let field = |name: &str| record[name].as_i64().unwrap();
            assert_eq!(
                tokens.input,
                Some(field("input_tokens")),
                "{version}/{scenario} step {step}"
            );
            assert_eq!(
                tokens.cache_read,
                Some(field("cache_read_tokens")),
                "{version}/{scenario} step {step}"
            );
            assert_eq!(
                tokens.output.unwrap() + tokens.thinking.unwrap(),
                field("output_tokens"),
                "{version}/{scenario} step {step}"
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 7);
}

/// The request grain reproduces the generation grain: summing the samples of
/// each generation's `step_indices` gives that generation's own `usage`.
#[test]
fn step_samples_sum_to_their_generation() {
    let mut generations = 0;
    for (version, scenario, db) in conversation_dbs() {
        let store = framed(&db);
        let (ops, _) = normalize(&store);
        let by_step: BTreeMap<u32, &SessionUsageRow> = samples(&ops)
            .into_iter()
            .map(|sample| {
                let session = &sample.session_id;
                let step = (0..64)
                    .find(|step| {
                        sample.key
                            == ids::usage_id(session, &format!("conversation-db:step#{step}"))
                    })
                    .expect("sample keyed by a step index");
                (step, sample)
            })
            .collect();
        for (query, record) in &store.records {
            if query != "gen_metadata" {
                continue;
            }
            let envelope: SnapshotRecord = serde_json::from_slice(record).unwrap();
            let ParsedRow::GenMetadata(row) = parse_row(&envelope.query, envelope.row).unwrap()
            else {
                unreachable!()
            };
            let generation =
                GeneratorMetadata::decode(&row.data.unwrap().bytes().unwrap()).unwrap();
            let usage = generation
                .chat_model
                .unwrap()
                .usage
                .expect("generation usage");
            let (mut input, mut output, mut thinking, mut cache_read) = (0, 0, 0, 0);
            for step in &generation.step_indices {
                if let Some(sample) = by_step.get(step) {
                    input += sample.tokens.input.unwrap();
                    output += sample.tokens.output.unwrap();
                    thinking += sample.tokens.thinking.unwrap();
                    cache_read += sample.tokens.cache_read.unwrap();
                }
            }
            assert_eq!(input as u64, usage.input_tokens, "{version}/{scenario}");
            assert_eq!(
                output as u64, usage.response_output_tokens,
                "{version}/{scenario}"
            );
            assert_eq!(
                thinking as u64, usage.thinking_output_tokens,
                "{version}/{scenario}"
            );
            assert_eq!(
                cache_read as u64, usage.cache_read_tokens,
                "{version}/{scenario}"
            );
            assert_eq!(
                (thinking + output) as u64,
                usage.output_tokens,
                "{version}/{scenario}: output_tokens = thinking + response"
            );
            generations += 1;
        }
    }
    // 1.1.20: five generations; 1.4.12: one per planner step, seven.
    assert_eq!(generations, 12);
}

#[test]
fn golden_ops_match() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for (version, scenario, db) in conversation_dbs() {
        let (ops, unhealthy) = normalize(&framed(&db));
        assert!(
            unhealthy.is_empty(),
            "{version}/{scenario} did not fully normalize:\n{}",
            unhealthy.join("\n")
        );
        assert!(!ops.is_empty(), "{version}/{scenario} produced no ops");

        let golden = golden_dir()
            .join(&version)
            .join(format!("{scenario}.conversation-db.json"));
        let rendered = format!("{}\n", serde_json::to_string_pretty(&ops).unwrap());
        if update {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &rendered).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_else(|error| {
            panic!(
                "{}: {error} — regenerate with UPDATE_GOLDEN=1 cargo test \
                 -p rosetta-adapter-antigravity --test conversation_db",
                golden.display()
            )
        });
        assert_eq!(
            rendered, expected,
            "normalized usage changed for {version}/{scenario}"
        );
    }
}

#[test]
fn replay_is_deterministic() {
    for (version, scenario, db) in conversation_dbs() {
        let first = normalize(&framed(&db)).0;
        let second = normalize(&framed(&db)).0;
        assert_eq!(first, second, "{version}/{scenario} replayed differently");
    }
}
