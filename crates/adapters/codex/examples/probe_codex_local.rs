//! Corroboration probe: normalize every live Codex rollout without opening a
//! database. Local data corroborates, never proves; this answers whether an
//! adapter revision survives historical writers before a costly full ingest.
//!
//! Run: `cargo run -p rosetta-adapter-codex --example probe_codex_local --release`

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::PathBuf;

use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::adapter::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::{IngestOp, ids};

fn main() {
    let root = std::env::var("CODEX_HOME")
        .unwrap_or_else(|_| format!("{}/.codex", std::env::var("HOME").expect("HOME")));
    let runtime_id = ids::runtime_id("codex", &root);
    let adapter = CodexAdapter;

    let mut files = 0u64;
    let mut lines = 0u64;
    let mut parsed = 0u64;
    let mut unknown = 0u64;
    let mut hard = 0u64;
    let mut ops = 0u64;
    let mut drift: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut hard_shapes: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut hard_samples = Vec::new();

    let mut stack = vec![PathBuf::from(&root).join("sessions")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_rollout = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"));
            if !is_rollout {
                continue;
            }

            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            files += 1;
            let native = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy();
            let session_id = ids::session_id(&runtime_id, &native);
            let mut ctx = NormalizeCtx::new(runtime_id.clone(), session_id);
            ctx.state = adapter.init_state();
            let mut reader = std::io::BufReader::new(file);
            let mut buffer = Vec::new();
            let mut offset = 0i64;
            let mut line_number = 0u64;

            loop {
                buffer.clear();
                let Ok(read) = reader.read_until(b'\n', &mut buffer) else {
                    break;
                };
                if read == 0 {
                    break;
                }
                line_number += 1;
                lines += 1;
                ctx.src_offset = Some(offset);
                offset += read as i64;
                if buffer.last() == Some(&b'\n') {
                    buffer.pop();
                }
                if buffer.last() == Some(&b'\r') {
                    buffer.pop();
                }

                let outcome = adapter.ingest_line(&mut ctx, "rollout", &buffer);
                match &outcome.status {
                    LineStatus::Parsed => parsed += 1,
                    LineStatus::UnknownType => unknown += 1,
                    LineStatus::HardError { code } => {
                        hard += 1;
                        *hard_shapes
                            .entry((code.clone(), failure_shape(&buffer)))
                            .or_default() += 1;
                        if hard_samples.len() < 20 {
                            hard_samples.push(format!(
                                "{}:{line_number}: {code}",
                                path.strip_prefix(&root).unwrap_or(&path).display()
                            ));
                        }
                    }
                }
                ops += outcome.ops.len() as u64;
                for op in outcome.ops {
                    if let IngestOp::Drift(fact) = op {
                        *drift
                            .entry((fact.error_code, fact.record_kind))
                            .or_default() += 1;
                    }
                }
            }
        }
    }

    println!("files: {files}");
    println!("lines: {lines}");
    println!("parsed: {parsed}");
    println!("unknown: {unknown}");
    println!("hard failures: {hard}");
    println!("ops: {ops}");
    for sample in hard_samples {
        println!("  {sample}");
    }
    println!("hard failure shapes:");
    for ((code, shape), count) in hard_shapes {
        println!("  {count} {code}: {shape}");
    }
    println!("drift (top 40):");
    let mut drift = drift.into_iter().collect::<Vec<_>>();
    drift.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for ((code, kind), count) in drift.into_iter().take(40) {
        println!("  {code}/{kind}: {count}");
    }
}

fn failure_shape(line: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return "<invalid-json>".to_owned();
    };
    let record_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<untyped>");
    let Some(payload) = value.get("payload").and_then(serde_json::Value::as_object) else {
        return format!("{record_type} payload={}", value_kind(value.get("payload")));
    };
    let subtype = payload
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<none>");
    let mut fields = payload
        .iter()
        .map(|(key, value)| format!("{key}={}", value_kind(Some(value))))
        .collect::<Vec<_>>();
    fields.sort();
    if let Some(blocks) = payload.get("content").and_then(serde_json::Value::as_array) {
        let mut block_shapes = blocks
            .iter()
            .map(|block| match block.as_object() {
                Some(block) => {
                    let mut fields = block
                        .iter()
                        .map(|(key, value)| format!("{key}={}", value_kind(Some(value))))
                        .collect::<Vec<_>>();
                    fields.sort();
                    fields.join(",")
                }
                None => value_kind(Some(block)).to_owned(),
            })
            .collect::<Vec<_>>();
        block_shapes.sort();
        block_shapes.dedup();
        fields.push(format!("content_blocks=[{}]", block_shapes.join(";")));
    }
    format!("{record_type}/{subtype} {}", fields.join(","))
}

fn value_kind(value: Option<&serde_json::Value>) -> &'static str {
    match value {
        None => "absent",
        Some(serde_json::Value::Null) => "null",
        Some(serde_json::Value::Bool(_)) => "bool",
        Some(serde_json::Value::Number(_)) => "number",
        Some(serde_json::Value::String(_)) => "string",
        Some(serde_json::Value::Array(_)) => "array",
        Some(serde_json::Value::Object(_)) => "object",
    }
}
