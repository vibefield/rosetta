//! The census-documented vocabulary the raw types must know.
//!
//! Claude Code is a closed writer. The fields writers 2.1.257–2.1.283 added
//! are proven by the organic structure census pinned to writer 2.1.283
//! (`sources/observed-2026-09-28.json`), not by a farmed fixture: the pinned
//! fixture corpus is 2.1.241. These tests hold the parser to the canonical
//! ground truth instead. Every field it documents for a typed record or the
//! subagent sidecar is a known field, and records carrying the new fields —
//! shaped by the census, values synthetic — normalize exactly as they do
//! without them and raise no drift.

use std::collections::BTreeMap;
use std::path::Path;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_claude_code::raw::{ParsedLine, parse_line, parse_subagent_meta};
use rosetta_adapter_support::{AgentAdapter, Drift, NormalizeCtx};
use rosetta_core::{IngestOp, ids};
use serde_json::{Map, Value, json};

fn canonical() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../groundtruth/claude-code/groundtruth.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The object shapes at `path` inside one record shape (`any_of` expanded).
fn objects_at<'a>(shape: &'a Value, path: &[&str], out: &mut Vec<&'a Value>) {
    match shape["kind"].as_str() {
        Some("any_of") => {
            for variant in shape["variants"].as_array().into_iter().flatten() {
                objects_at(variant, path, out);
            }
        }
        Some("object") => match path.split_first() {
            None => out.push(shape),
            Some((head, rest)) => {
                if let Some(field) = shape["fields"].get(*head) {
                    objects_at(&field["shape"], rest, out);
                }
            }
        },
        _ => {}
    }
}

/// The two streams whose record vocabularies the adapter unions.
const TRANSCRIPTS: &[&str] = &["transcript", "subagent-transcript"];

/// Field name → every shape the canonical documents for it, at `path` in
/// `kind`, over `streams`.
fn documented(
    canonical: &Value,
    streams: &[&str],
    kind: &str,
    path: &[&str],
) -> BTreeMap<String, Vec<Value>> {
    let mut fields = BTreeMap::<String, Vec<Value>>::new();
    for stream in canonical["streams"].as_array().unwrap() {
        if !stream["id"]
            .as_str()
            .is_some_and(|id| streams.contains(&id))
        {
            continue;
        }
        let Some(record) = stream["record_types"].get(kind) else {
            continue;
        };
        let mut objects = Vec::new();
        objects_at(&record["shape"], path, &mut objects);
        for object in objects {
            for (name, field) in object["fields"].as_object().into_iter().flatten() {
                fields
                    .entry(name.clone())
                    .or_default()
                    .push(field["shape"].clone());
            }
        }
    }
    fields
}

/// The simplest value of each variant a documented shape admits.
fn samples(shape: &Value) -> Vec<Value> {
    match shape["kind"].as_str() {
        Some("string") => vec![Value::String(
            shape["values"]
                .as_object()
                .and_then(|values| values.keys().next().cloned())
                .unwrap_or_else(|| "x".to_owned()),
        )],
        Some("number") => vec![json!(0)],
        Some("boolean") => vec![json!(false)],
        Some("array") => vec![json!([])],
        Some("object") => {
            let mut object = Map::new();
            for (name, field) in shape["fields"].as_object().into_iter().flatten() {
                if field["required"] == true && !name.starts_with('<') {
                    object.insert(name.clone(), samples(&field["shape"]).remove(0));
                }
            }
            vec![Value::Object(object)]
        }
        Some("any_of") => shape["variants"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(samples)
            .collect(),
        _ => vec![Value::Null],
    }
}

fn envelope(kind: &str) -> Value {
    json!({
        "type": kind,
        "parentUuid": null,
        "isSidechain": false,
        "uuid": "11111111-1111-1111-1111-111111111111",
        "timestamp": "2026-09-28T00:00:00.000Z",
        "sessionId": "session-under-test",
        "version": "2.1.283",
        "cwd": "/tmp/project",
        "gitBranch": "main",
        "userType": "external",
    })
}

/// A minimal valid record of every type the raw types parse.
fn typed_records() -> Vec<(&'static str, Value)> {
    let mut user = envelope("user");
    user["message"] = json!({"role": "user", "content": "hello"});
    let mut assistant = envelope("assistant");
    assistant["message"] = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-5-5",
        "content": [],
        "stop_reason": null,
        "stop_sequence": null,
        "usage": {
            "input_tokens": 1,
            "output_tokens": 1,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
            "cache_creation": {},
            "inference_geo": null,
            "service_tier": null,
        },
    });
    let mut system = envelope("system");
    system["subtype"] = json!("turn_duration");
    vec![
        ("user", user),
        ("assistant", assistant),
        ("system", system),
        (
            "ai-title",
            json!({"type": "ai-title", "aiTitle": "t", "sessionId": "s"}),
        ),
        (
            "custom-title",
            json!({"type": "custom-title", "customTitle": "t", "sessionId": "s"}),
        ),
        (
            "atis-latch",
            json!({"type": "atis-latch", "atis": "a", "sessionId": "s"}),
        ),
        (
            "queue-operation",
            json!({
                "type": "queue-operation",
                "operation": "enqueue",
                "timestamp": "2026-09-28T00:00:00.000Z",
                "sessionId": "s",
            }),
        ),
        (
            "last-prompt",
            json!({"type": "last-prompt", "sessionId": "s"}),
        ),
        (
            "mode",
            json!({"type": "mode", "mode": "default", "sessionId": "s"}),
        ),
        (
            "cost-state",
            json!({"type": "cost-state", "sessionId": "s", "totalCostUSD": 0,
                "totalAPIDuration": 0, "totalAPIDurationWithoutRetries": 0,
                "totalToolDuration": 0, "totalLinesAdded": 0, "totalLinesRemoved": 0,
                "totalDuration": 0, "startTime": 0, "hasUnknownModelCost": false,
                "modelUsage": {}}),
        ),
    ]
}

/// Adding a census field to the canonical without typing it would turn every
/// record that carries it into a drift fact: fail here instead.
#[test]
fn typed_records_know_every_field_the_canonical_documents() {
    let canonical = canonical();
    let mut untyped = Vec::new();
    let mut unparsable = Vec::new();
    for (kind, record) in typed_records() {
        // The levels whose unknown keys the raw types report as drift.
        let levels: &[&[&str]] = match kind {
            "user" => &[&[], &["message"], &["origin"]],
            "assistant" => &[&[], &["message"], &["message", "usage"]],
            _ => &[&[]],
        };
        for path in levels {
            let mut base = record.clone();
            if *path == ["origin"] {
                base["origin"] = json!({"kind": "human"});
            }
            let present = path.iter().fold(&base, |value, key| &value[*key]).clone();
            let prefix: String = path.iter().map(|key| format!(".{key}")).collect();
            for (name, shapes) in documented(&canonical, TRANSCRIPTS, kind, path) {
                if name.starts_with('<') || present.get(&name).is_some() {
                    continue;
                }
                for value in shapes.iter().flat_map(samples) {
                    let mut line = base.clone();
                    let parent = path.iter().fold(&mut line, |value, key| &mut value[*key]);
                    parent[&name] = value.clone();
                    let at = format!("{kind}{prefix}.{name}");
                    match parse_line(&line.to_string()) {
                        Ok(ParsedLine::Known(parsed)) => {
                            if parsed.drift_paths().contains(&format!("{prefix}.{name}")) {
                                untyped.push(at);
                            }
                        }
                        other => unparsable.push(format!("{at} = {value}: {other:?}")),
                    }
                }
            }
        }
    }
    untyped.dedup();
    assert!(
        untyped.is_empty(),
        "documented by groundtruth.json but not typed: {untyped:?}"
    );
    assert!(
        unparsable.is_empty(),
        "documented shapes the raw types reject: {unparsable:?}"
    );
}

/// The subagent sidecar's raw type keeps every census field typed, so an
/// addition cannot masquerade as known metadata.
#[test]
fn subagent_metadata_knows_every_field_the_canonical_documents() {
    let canonical = canonical();
    let mut untyped = Vec::new();
    for (name, shapes) in documented(&canonical, &["subagent-metadata"], "", &[]) {
        if name.starts_with('<') || name == "agentType" {
            continue;
        }
        for value in shapes.iter().flat_map(samples) {
            let mut document = json!({"agentType": "Explore"});
            document[&name] = value.clone();
            let parsed = parse_subagent_meta(&document.to_string())
                .unwrap_or_else(|error| panic!("{name} = {value}: {error}"));
            if !parsed.drift_paths().is_empty() {
                untyped.push(name.clone());
            }
        }
    }
    untyped.dedup();
    assert!(
        untyped.is_empty(),
        "documented by groundtruth.json but not typed: {untyped:?}"
    );
}

fn ingest(lines: &[Value]) -> Vec<IngestOp> {
    let adapter = ClaudeCodeAdapter;
    let runtime_id = ids::runtime_id(adapter.kind(), &adapter.layout().data_root);
    let mut ctx = NormalizeCtx::new(
        &runtime_id,
        ids::session_id(&runtime_id, "session-under-test"),
    );
    ctx.state = adapter.init_state();
    lines
        .iter()
        .flat_map(|line| {
            adapter
                .ingest_line(&mut ctx, "transcript", line.to_string().as_bytes())
                .ops
        })
        .collect()
}

fn with(mut record: Value, fields: Value) -> Value {
    let (Value::Object(record_fields), Value::Object(fields)) = (&mut record, fields) else {
        unreachable!("both are objects");
    };
    record_fields.extend(fields);
    record
}

/// One exchange as writer 2.1.241 records it: a Bash call, its result, a
/// slash-command echo, a queue operation, an attachment and a bridge marker.
fn exchange() -> Vec<Value> {
    let mut assistant = envelope("assistant");
    assistant["requestId"] = json!("req_1");
    assistant["message"] = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-5-5",
        "content": [{
            "type": "tool_use",
            "id": "toolu_1",
            "name": "Bash",
            "input": {"command": "true", "description": "d"},
        }],
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 3,
            "output_tokens": 5,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
            "cache_creation": {},
            "inference_geo": null,
            "service_tier": "standard",
        },
    });
    let mut user = envelope("user");
    user["uuid"] = json!("22222222-2222-2222-2222-222222222222");
    user["parentUuid"] = json!("11111111-1111-1111-1111-111111111111");
    user["timestamp"] = json!("2026-09-28T00:00:01.000Z");
    user["message"] = json!({
        "role": "user",
        "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}],
    });
    user["toolUseResult"] = json!({"stdout": "ok", "stderr": "", "interrupted": false});
    let mut system = envelope("system");
    system["uuid"] = json!("33333333-3333-3333-3333-333333333333");
    system["timestamp"] = json!("2026-09-28T00:00:02.000Z");
    system["subtype"] = json!("local_command");
    system["content"] = json!("<command-name>/effort</command-name>");
    system["level"] = json!("info");
    vec![
        assistant,
        user,
        system,
        json!({
            "type": "queue-operation",
            "operation": "remove",
            "timestamp": "2026-09-28T00:00:03.000Z",
            "sessionId": "session-under-test",
        }),
        json!({"type": "attachment", "attachment": {"type": "skill_listing"}}),
        json!({"type": "bridge-session", "sessionId": "session-under-test"}),
    ]
}

#[test]
fn fields_added_by_writers_2_1_257_to_2_1_283_are_ledger_only() {
    let plain = exchange();
    let mut dressed = exchange();
    dressed[0] = with(
        dressed[0].clone(),
        json!({
            "apiBlockIndex": 0,
            "perTurnEffort": "x",
            "advisorModel": "claude-opus-5-5",
            "serverClassifierRequest": "x",
            "wireToolInputs": {"toolu_1": {"command": "true", "description": "d"}},
            "wireIngestContext": {"toolu_1": {"cwd": "/tmp/project"}},
        }),
    );
    dressed[0]["message"]["input_transformations"] = json!([]);
    dressed[1] = with(
        dressed[1].clone(),
        json!({
            "serverClassifierContext": {
                "context": {"git_state": {}, "live_cwd": "/tmp/project", "platform": "x"},
                "request": "x",
            },
            "turnOrigin": "x",
        }),
    );
    dressed[2] = with(
        dressed[2].clone(),
        json!({"commandRun": {"command": "effort", "args": ""}, "teamName": "x"}),
    );
    dressed[3] = with(dressed[3].clone(), json!({"reason": "absorbed_mid_turn"}));
    dressed[4] = with(dressed[4].clone(), json!({"rendered": [{"content": "x"}]}));
    dressed[5] = with(dressed[5].clone(), json!({"noHistoryBackfill": true}));

    let ops = ingest(&dressed);
    assert!(
        !ops.iter().any(|op| matches!(op, IngestOp::Drift(_))),
        "{ops:#?}"
    );
    assert_eq!(ops, ingest(&plain), "the new fields project nothing");

    // Knowing these fields is not a blanket allowance: an undocumented one
    // still raises drift on the same record.
    dressed[0] = with(dressed[0].clone(), json!({"notYetDocumented": 1}));
    let drift = ingest(&dressed)
        .into_iter()
        .filter(|op| matches!(op, IngestOp::Drift(_)))
        .count();
    assert_eq!(drift, 1);
}
