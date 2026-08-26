//! claude-code corpus: `projects/<slug>/<session-uuid>.jsonl`.
//!
//! SPEC (iron rule 1): `groundtruth/claude-code/groundtruth.json`, stream
//! `transcript`, plus the raw types in
//! `crates/adapters/claude-code/src/raw/transcript.rs`. Every record emitted
//! here is one of the six typed record kinds — `user`, `assistant`,
//! `atis-latch`, `queue-operation`, `last-prompt`, `mode` — carrying exactly
//! the fields those types name. Nothing is invented: a field appears below
//! only because the canonical document observed it.
//!
//! Deliberately NOT generated: `attachment`, `progress`, `system`, `ai-title`,
//! `summary`, `file-history-*`, `bridge-session`. The canonical document lists
//! them, but the adapter has not typed them yet, so they parse as soft
//! `UnknownType` drift. Emitting them would make the self-check assert against
//! its own noise; they become one match arm each the day the adapter types
//! them.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::clock::{self, BASE_EPOCH_MS, MS_PER_DAY};
use crate::prng::Rng;
use crate::report::{LineSink, StreamReport};
use crate::{CorpusSpec, project_count, synth};

/// Writer versions stamped per record. A spread of them is realistic (organic
/// corpora span ~40) and keeps version-aware code honest.
const VERSIONS: &[&str] = &["2.1.239", "2.1.240", "2.1.241"];
const MODELS: &[&str] = &[
    "claude-opus-5",
    "claude-sonnet-5",
    "claude-haiku-4-5-20251001",
];
const BRANCHES: &[&str] = &["main", "HEAD", "bench/topic-a", "bench/topic-b"];
const ENTRYPOINTS: &[&str] = &["cli", "sdk-cli"];
const EFFORTS: &[&str] = &["high", "medium", "low"];
const PERMISSION_MODES: &[&str] = &["default", "acceptEdits", "plan", "bypassPermissions"];
const TOOLS: &[&str] = &["Bash", "Read", "Edit", "Write", "Grep", "Glob"];

/// Sessions are dated across this window so time-bucketed queries (heatmaps,
/// contribution graphs) have something to bucket.
const SPREAD_DAYS: u64 = 180;

pub fn generate(spec: &CorpusSpec, counts: &[u64], out_dir: &Path) -> io::Result<StreamReport> {
    let projects = project_count(counts.len());
    let mut report = StreamReport {
        stream: "transcript".to_string(),
        files: 0,
        lines: 0,
        bytes: 0,
    };

    for (index, &budget) in counts.iter().enumerate() {
        let mut rng = Rng::stream(spec.seed, index as u64 + 1);
        let mut session = Session::new(&mut rng, projects);

        let dir = out_dir.join("projects").join(&session.slug);
        fs::create_dir_all(&dir)?;
        let file = fs::File::create(dir.join(format!("{}.jsonl", session.id)))?;

        let mut sink = LineSink::new(BufWriter::with_capacity(256 * 1024, file));
        session.write(&mut sink, &mut rng, budget, spec.tool_ratio)?;
        let (lines, bytes) = sink.finish()?;

        report.files += 1;
        report.lines += lines;
        report.bytes += bytes;
    }
    Ok(report)
}

/// Everything a transcript stamps on every line, plus the walking state that
/// makes a session a coherent conversation rather than a bag of records.
struct Session {
    id: String,
    cwd: String,
    slug: String,
    version: &'static str,
    model: &'static str,
    branch: &'static str,
    entrypoint: &'static str,
    effort: &'static str,
    permission_mode: &'static str,
    clock_ms: i64,
    /// The uuid of the previous timeline record — the `parentUuid` chain.
    parent: Option<String>,
    /// Leaf of the chain, for the closing `last-prompt` record.
    leaf: String,
    turn: u64,
    tool_seq: u64,
    input_tokens: u64,
    cache_read: u64,
    last_prompt: String,
}

impl Session {
    fn new(rng: &mut Rng, projects: u64) -> Self {
        let cwd = synth::project_cwd(rng.below(projects));
        Self {
            id: rng.uuid(4),
            slug: synth::project_slug(&cwd),
            cwd,
            version: rng.pick(VERSIONS),
            model: rng.pick(MODELS),
            branch: rng.pick(BRANCHES),
            entrypoint: rng.pick(ENTRYPOINTS),
            effort: rng.pick(EFFORTS),
            permission_mode: rng.pick(PERMISSION_MODES),
            clock_ms: BASE_EPOCH_MS + rng.below(SPREAD_DAYS * MS_PER_DAY as u64) as i64,
            parent: None,
            leaf: String::new(),
            turn: 0,
            tool_seq: 0,
            input_tokens: rng.range(1_200, 9_000),
            cache_read: 0,
            last_prompt: String::new(),
        }
    }

    /// Emit exactly `budget` lines (>= [`crate::Agent::min_session_lines`]).
    ///
    /// Shape: an `atis-latch` header, alternating user/assistant turns — a
    /// share of them driving tool round-trips — and a `last-prompt` leaf
    /// marker. The budget is spent greedily and the remainder, which can only
    /// ever be one line, becomes a standalone `mode` record.
    fn write<W: Write>(
        &mut self,
        sink: &mut LineSink<W>,
        rng: &mut Rng,
        budget: u64,
        tool_ratio: f64,
    ) -> io::Result<()> {
        sink.write(&self.atis_latch(rng))?;
        // One line reserved for the header, one for the trailing marker.
        let mut body = budget - 2;

        while body >= 2 {
            let prompt_id = rng.uuid(4);
            sink.write(&self.user_prompt(rng, &prompt_id))?;
            body -= 1;

            if rng.chance(tool_ratio) {
                let mut round = 0;
                // A round is assistant(tool_use) + user(tool_result); the
                // closing assistant text still has to fit after it.
                while body >= 3 && round < 4 && (round == 0 || rng.chance(0.45)) {
                    let (record, tool_id) = self.assistant_tool_use(rng);
                    let call_uuid = self.leaf.clone();
                    sink.write(&record)?;
                    sink.write(&self.tool_result(rng, &prompt_id, &tool_id, &call_uuid))?;
                    body -= 2;
                    round += 1;
                }
            }

            sink.write(&self.assistant_text(rng))?;
            body -= 1;
            self.turn += 1;

            if body >= 2 && rng.chance(0.05) {
                sink.write(&self.queue_operation(rng, "enqueue"))?;
                sink.write(&self.queue_operation(rng, "dequeue"))?;
                body -= 2;
            }
        }

        while body > 0 {
            sink.write(&self.mode(rng))?;
            body -= 1;
        }
        sink.write(&self.last_prompt())?;
        Ok(())
    }

    /// Advance the session clock by a plausible think/act gap.
    fn tick(&mut self, rng: &mut Rng) -> String {
        self.clock_ms += rng.range(180, 9_000) as i64;
        clock::iso8601(self.clock_ms)
    }

    /// Mint the next timeline uuid and walk the parent chain onto it.
    fn link(&mut self, rng: &mut Rng) -> (Value, String) {
        let parent = match &self.parent {
            Some(uuid) => Value::String(uuid.clone()),
            None => Value::Null,
        };
        let uuid = rng.uuid(4);
        self.parent = Some(uuid.clone());
        self.leaf = uuid.clone();
        (parent, uuid)
    }

    /// Fields every `user`/`assistant` line carries (canonical: required on
    /// 100% of observed records).
    fn envelope(&self, timestamp: &str) -> Vec<(&'static str, Value)> {
        vec![
            ("timestamp", json!(timestamp)),
            ("isSidechain", json!(false)),
            ("userType", json!("external")),
            ("entrypoint", json!(self.entrypoint)),
            ("cwd", json!(self.cwd)),
            ("sessionId", json!(self.id)),
            ("version", json!(self.version)),
            ("gitBranch", json!(self.branch)),
        ]
    }

    fn record(&self, base: Value, timestamp: &str) -> Value {
        let mut object = base;
        let map = object.as_object_mut().expect("record is an object");
        for (key, value) in self.envelope(timestamp) {
            map.insert(key.to_string(), value);
        }
        object
    }

    fn atis_latch(&self, rng: &mut Rng) -> Value {
        json!({ "type": "atis-latch", "atis": rng.hex(40), "sessionId": self.id })
    }

    fn user_prompt(&mut self, rng: &mut Rng, prompt_id: &str) -> Value {
        let timestamp = self.tick(rng);
        let (parent, uuid) = self.link(rng);
        let prompt = synth::text(rng, "prompt", self.turn, 6, 120);
        self.last_prompt = prompt.clone();
        self.record(
            json!({
                "type": "user",
                "parentUuid": parent,
                "uuid": uuid,
                "promptId": prompt_id,
                "promptSource": if self.entrypoint == "cli" { "cli" } else { "sdk" },
                "permissionMode": self.permission_mode,
                "message": { "role": "user", "content": prompt },
            }),
            &timestamp,
        )
    }

    fn assistant_text(&mut self, rng: &mut Rng) -> Value {
        let mut blocks = Vec::new();
        if rng.chance(0.22) {
            blocks.push(json!({
                "type": "thinking",
                "thinking": synth::text(rng, "reasoning", self.turn, 20, 220),
                "signature": rng.hex(96),
            }));
        }
        blocks.push(json!({
            "type": "text",
            "text": synth::text(rng, "assistant response", self.turn, 15, 300),
        }));
        self.assistant(rng, blocks, "end_turn")
    }

    /// Returns the record and the `tool_use` id its result must reference.
    fn assistant_tool_use(&mut self, rng: &mut Rng) -> (Value, String) {
        let tool = *rng.pick(TOOLS);
        self.tool_seq += 1;
        let tool_id = format!("toolu_synth_{}_{:04}", &self.id[..8], self.tool_seq);

        let mut blocks = Vec::new();
        if rng.chance(0.5) {
            blocks.push(json!({
                "type": "text",
                "text": synth::text(rng, "tool preamble", self.turn, 8, 60),
            }));
        }
        blocks.push(json!({
            "type": "tool_use",
            "id": tool_id,
            "name": tool,
            "input": self.tool_input(rng, tool),
        }));
        (self.assistant(rng, blocks, "tool_use"), tool_id)
    }

    fn assistant(&mut self, rng: &mut Rng, blocks: Vec<Value>, stop_reason: &str) -> Value {
        let timestamp = self.tick(rng);
        let (parent, uuid) = self.link(rng);
        // Context grows across a session; cache reads dominate once it is warm.
        let fresh = rng.range(40, 900);
        let cache_creation = if self.cache_read == 0 {
            self.input_tokens
        } else {
            rng.range(0, 600)
        };
        self.cache_read += cache_creation;
        self.input_tokens += fresh;
        let output = rng.range(24, 1_400);

        self.record(
            json!({
                "type": "assistant",
                "parentUuid": parent,
                "uuid": uuid,
                "effort": self.effort,
                "requestId": format!("req_synth_{}", rng.hex(24)),
                "message": {
                    "id": format!("msg_synth_{}", rng.hex(24)),
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": blocks,
                    "stop_reason": stop_reason,
                    "stop_sequence": Value::Null,
                    "stop_details": Value::Null,
                    "usage": {
                        "input_tokens": fresh,
                        "cache_creation_input_tokens": cache_creation,
                        "cache_read_input_tokens": self.cache_read,
                        "output_tokens": output,
                        "output_tokens_details": { "thinking_tokens": rng.below(output / 2) },
                        "server_tool_use": {
                            "web_search_requests": 0,
                            "web_fetch_requests": 0,
                        },
                        "service_tier": "standard",
                        "cache_creation": {
                            "ephemeral_1h_input_tokens": 0,
                            "ephemeral_5m_input_tokens": cache_creation,
                        },
                        "inference_geo": "",
                        "iterations": [],
                        "speed": "standard",
                    },
                },
            }),
            &timestamp,
        )
    }

    fn tool_result(
        &mut self,
        rng: &mut Rng,
        prompt_id: &str,
        tool_id: &str,
        call_uuid: &str,
    ) -> Value {
        let timestamp = self.tick(rng);
        let (parent, uuid) = self.link(rng);
        let is_error = rng.chance(0.06);
        let body = if is_error {
            format!("synthetic tool failure {}: exit status 1", rng.below(64))
        } else {
            synth::text(rng, "tool output", self.tool_seq, 5, 400)
        };
        let (stdout, stderr) = if is_error {
            (String::new(), body.clone())
        } else {
            (body.clone(), String::new())
        };
        self.record(
            json!({
                "type": "user",
                "parentUuid": parent,
                "uuid": uuid,
                "promptId": prompt_id,
                "sourceToolAssistantUUID": call_uuid,
                "sourceToolUseID": tool_id,
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_id,
                        "content": body,
                        "is_error": is_error,
                    }],
                },
                // Tool-dependent and deliberately unshaped — narrowing this per
                // tool is a known spaghetti production bug.
                "toolUseResult": {
                    "stdout": stdout,
                    "stderr": stderr,
                    "interrupted": false,
                    "isImage": false,
                },
            }),
            &timestamp,
        )
    }

    fn tool_input(&self, rng: &mut Rng, tool: &str) -> Value {
        match tool {
            "Bash" => json!({
                "command": synth::shell_command(rng, &self.cwd),
                "description": synth::text(rng, "command purpose", self.turn, 3, 12),
            }),
            "Read" => json!({ "file_path": synth::workspace_file(rng, &self.cwd) }),
            "Write" => json!({
                "file_path": synth::workspace_file(rng, &self.cwd),
                "content": synth::text(rng, "file body", self.turn, 20, 400),
            }),
            "Edit" => json!({
                "file_path": synth::workspace_file(rng, &self.cwd),
                "old_string": synth::text(rng, "before", self.turn, 5, 60),
                "new_string": synth::text(rng, "after", self.turn, 5, 60),
            }),
            "Grep" => json!({
                "pattern": format!("synthetic-{}", rng.below(1_000)),
                "path": self.cwd,
                "output_mode": "content",
            }),
            _ => json!({ "pattern": "**/*.rs", "path": self.cwd }),
        }
    }

    fn queue_operation(&mut self, rng: &mut Rng, operation: &str) -> Value {
        let timestamp = self.tick(rng);
        let mut record = json!({
            "type": "queue-operation",
            "operation": operation,
            "timestamp": timestamp,
            "sessionId": self.id,
        });
        if operation == "enqueue" {
            record["content"] = json!(synth::text(rng, "queued prompt", self.turn, 4, 30));
        }
        record
    }

    fn mode(&self, rng: &mut Rng) -> Value {
        json!({
            "type": "mode",
            "mode": rng.pick(PERMISSION_MODES),
            "sessionId": self.id,
        })
    }

    fn last_prompt(&self) -> Value {
        json!({
            "type": "last-prompt",
            "lastPrompt": self.last_prompt,
            "leafUuid": self.leaf,
            "sessionId": self.id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Session;
    use crate::prng::Rng;
    use crate::report::LineSink;
    use serde_json::Value;

    fn session_lines(budget: u64, seed: u64, tool_ratio: f64) -> Vec<Value> {
        let mut rng = Rng::new(seed);
        let mut session = Session::new(&mut rng, 4);
        let mut buffer = Vec::new();
        let mut sink = LineSink::new(&mut buffer);
        session
            .write(&mut sink, &mut rng, budget, tool_ratio)
            .unwrap();
        sink.finish().unwrap();
        String::from_utf8(buffer)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn spends_the_budget_exactly() {
        for budget in [4u64, 5, 6, 7, 11, 64, 501] {
            for seed in [1u64, 2, 3] {
                assert_eq!(session_lines(budget, seed, 0.3).len() as u64, budget);
            }
        }
    }

    #[test]
    fn opens_with_a_latch_and_closes_on_the_leaf() {
        let lines = session_lines(40, 5, 0.5);
        assert_eq!(lines[0]["type"], "atis-latch");
        let last = lines.last().unwrap();
        assert_eq!(last["type"], "last-prompt");
        let leaf = last["leafUuid"].as_str().unwrap();
        let final_turn = lines
            .iter()
            .rev()
            .find(|line| line["uuid"].is_string())
            .unwrap();
        assert_eq!(final_turn["uuid"], leaf);
    }

    #[test]
    fn the_parent_chain_is_unbroken() {
        let lines = session_lines(200, 11, 0.4);
        let mut previous: Option<String> = None;
        for line in &lines {
            let Some(uuid) = line["uuid"].as_str() else {
                continue; // marker records sit outside the chain
            };
            match &previous {
                None => assert!(line["parentUuid"].is_null(), "session must open on null"),
                Some(parent) => assert_eq!(line["parentUuid"], Value::String(parent.clone())),
            }
            previous = Some(uuid.to_string());
        }
        assert!(previous.is_some());
    }

    #[test]
    fn every_tool_use_has_its_result() {
        let lines = session_lines(400, 21, 1.0);
        let mut calls = Vec::new();
        let mut results = Vec::new();
        for line in &lines {
            let Some(blocks) = line["message"]["content"].as_array() else {
                continue;
            };
            for block in blocks {
                match block["type"].as_str() {
                    Some("tool_use") => calls.push(block["id"].as_str().unwrap().to_string()),
                    Some("tool_result") => {
                        results.push(block["tool_use_id"].as_str().unwrap().to_string())
                    }
                    _ => {}
                }
            }
        }
        assert!(!calls.is_empty());
        assert_eq!(calls, results);
    }

    #[test]
    fn the_tool_ratio_is_honoured() {
        assert_eq!(tool_use_blocks(&session_lines(600, 33, 0.0)), 0);
        assert!(tool_use_blocks(&session_lines(600, 33, 1.0)) > 20);
    }

    #[test]
    fn timestamps_advance_monotonically() {
        let lines = session_lines(300, 44, 0.3);
        let stamps: Vec<&str> = lines
            .iter()
            .filter_map(|line| line["timestamp"].as_str())
            .collect();
        assert!(stamps.len() > 10);
        assert!(stamps.windows(2).all(|pair| pair[0] < pair[1]));
    }

    fn tool_use_blocks(lines: &[Value]) -> usize {
        lines
            .iter()
            .filter_map(|line| line["message"]["content"].as_array())
            .flatten()
            .filter(|block| block["type"] == "tool_use")
            .count()
    }
}
