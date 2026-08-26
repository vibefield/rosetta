//! codex corpus: `sessions/YYYY/MM/DD/rollout-<stamp>-<uuid>.jsonl`.
//!
//! SPEC (iron rule 1): `groundtruth/codex/groundtruth.json`, stream `rollout`,
//! plus the raw types in `crates/adapters/codex/src/raw/rollout.rs`. Every line
//! is the `{timestamp, ordinal, type, payload}` envelope over one of the five
//! typed payloads — `session_meta`, `event_msg`, `response_item`,
//! `turn_context`, `world_state`. Payload sub-vocabularies follow the canonical
//! document's facets: event_msg is task_started / item_completed / token_count
//! / task_complete, response_item is message / function_call /
//! function_call_output, and item_completed items are UserMessage /
//! AgentMessage / CommandExecution.
//!
//! Two properties the ground truth calls out and this generator reproduces
//! because downstream normalization depends on them:
//!
//! * `token_count.info.total_token_usage` is CUMULATIVE and rises across the
//!   session, while `last_token_usage` is the per-response delta. The adapter's
//!   cumulative→delta step is only exercised if this holds.
//! * `ordinal` is the line index within the file, starting at zero.
//!
//! Deliberately NOT generated: `compacted` (research-only, no fixture) and the
//! `thread_settings_applied` event. Both would parse, but neither has a shape
//! the canonical document pins down field-for-field.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::clock::{self, BASE_EPOCH_MS, MS_PER_DAY};
use crate::prng::Rng;
use crate::report::{LineSink, StreamReport};
use crate::{CorpusSpec, project_count, synth};

const CLI_VERSIONS: &[&str] = &["0.149.0", "0.149.1"];
const MODELS: &[&str] = &["synthetic-model-a", "synthetic-model-b"];
const ORIGINATORS: &[(&str, &str)] = &[("codex_exec", "exec"), ("codex_tui", "interactive")];
const PERSONALITIES: &[&str] = &["pragmatic", "concise", "friendly"];
const APPROVAL_POLICIES: &[&str] = &["never", "on-request", "on-failure"];
const CONTEXT_WINDOW: u64 = 258_400;
const SPREAD_DAYS: u64 = 180;

pub fn generate(spec: &CorpusSpec, counts: &[u64], out_dir: &Path) -> io::Result<StreamReport> {
    let projects = project_count(counts.len());
    let mut report = StreamReport {
        stream: "rollout".to_string(),
        files: 0,
        lines: 0,
        bytes: 0,
    };

    for (index, &budget) in counts.iter().enumerate() {
        let mut rng = Rng::stream(spec.seed, index as u64 + 1);
        let mut session = Session::new(&mut rng, projects);

        let (year, month, day) = clock::date_parts(session.clock_ms);
        let dir = out_dir.join("sessions").join(year).join(month).join(day);
        fs::create_dir_all(&dir)?;
        let name = format!(
            "rollout-{}-{}.jsonl",
            clock::file_stamp(session.clock_ms),
            session.id
        );
        let file = fs::File::create(dir.join(name))?;

        let mut sink = LineSink::new(BufWriter::with_capacity(256 * 1024, file));
        session.write(&mut sink, &mut rng, budget, spec.tool_ratio)?;
        let (lines, bytes) = sink.finish()?;

        report.files += 1;
        report.lines += lines;
        report.bytes += bytes;
    }
    Ok(report)
}

/// Cumulative token usage. Codex reports the running total on every
/// `token_count` event; the delta is what the adapter has to recover.
#[derive(Default, Clone, Copy)]
struct Tokens {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
}

impl Tokens {
    fn total(&self) -> u64 {
        self.input + self.output
    }

    fn value(&self) -> Value {
        json!({
            "input_tokens": self.input,
            "cached_input_tokens": self.cached,
            "cache_write_input_tokens": self.cache_write,
            "output_tokens": self.output,
            "reasoning_output_tokens": self.reasoning,
            "total_tokens": self.total(),
        })
    }

    fn add(&mut self, delta: Tokens) {
        self.input += delta.input;
        self.cached += delta.cached;
        self.cache_write += delta.cache_write;
        self.output += delta.output;
        self.reasoning += delta.reasoning;
    }
}

struct Session {
    id: String,
    cwd: String,
    cli_version: &'static str,
    model: &'static str,
    originator: &'static str,
    source: &'static str,
    personality: &'static str,
    approval_policy: &'static str,
    clock_ms: i64,
    turn_id: String,
    turn: u64,
    call_seq: u64,
    total: Tokens,
    last: Tokens,
    last_agent_message: String,
    turn_started_ms: i64,
}

impl Session {
    fn new(rng: &mut Rng, projects: u64) -> Self {
        let (originator, source) = *rng.pick(ORIGINATORS);
        Self {
            id: rng.uuid(7),
            cwd: synth::project_cwd(rng.below(projects)),
            cli_version: rng.pick(CLI_VERSIONS),
            model: rng.pick(MODELS),
            originator,
            source,
            personality: rng.pick(PERSONALITIES),
            approval_policy: rng.pick(APPROVAL_POLICIES),
            clock_ms: BASE_EPOCH_MS + rng.below(SPREAD_DAYS * MS_PER_DAY as u64) as i64,
            turn_id: rng.uuid(7),
            turn: 0,
            call_seq: 0,
            total: Tokens::default(),
            last: Tokens::default(),
            last_agent_message: String::new(),
            turn_started_ms: 0,
        }
    }

    /// Emit exactly `budget` lines (>= [`crate::Agent::min_session_lines`]).
    ///
    /// Shape: the six-record session prelude, then turns bracketed by
    /// `task_started`/`task_complete`, a share of them driving tool
    /// round-trips. The remainder — never more than one turn's worth — becomes
    /// trailing `token_count` events, which is a shape codex genuinely emits.
    fn write<W: Write>(
        &mut self,
        sink: &mut LineSink<W>,
        rng: &mut Rng,
        budget: u64,
        tool_ratio: f64,
    ) -> io::Result<()> {
        self.prelude(sink, rng)?;
        let mut left = budget.saturating_sub(6);
        let mut first = true;

        loop {
            // A turn costs 6 lines, plus 2 more to re-open the context when it
            // is not the session's first.
            let overhead = if first { 0 } else { 2 };
            if left < overhead + 6 {
                break;
            }
            if !first {
                self.turn_id = rng.uuid(7);
                self.turn_started_ms = self.clock_ms;
                let context = self.turn_context();
                self.emit(sink, rng, "turn_context", context)?;
                let started = self.task_started();
                self.emit(sink, rng, "event_msg", started)?;
                left -= 2;
            }
            first = false;

            let prompt = synth::text(rng, "prompt", self.turn, 6, 120);
            let item = self.message(rng, "user", &prompt);
            self.emit(sink, rng, "response_item", item)?;
            let completed = self.item_completed(user_message_item(rng, &prompt));
            self.emit(sink, rng, "event_msg", completed)?;
            left -= 2;

            if rng.chance(tool_ratio) {
                let mut round = 0;
                // A round is 4 lines; the 4-line turn close must still fit.
                while left >= 8 && round < 4 && (round == 0 || rng.chance(0.45)) {
                    self.tool_round(sink, rng)?;
                    left -= 4;
                    round += 1;
                }
            }

            self.close_turn(sink, rng)?;
            left -= 4;
            self.turn += 1;
        }

        while left > 0 {
            let event = self.token_count(rng, Tokens::default());
            self.emit(sink, rng, "event_msg", event)?;
            left -= 1;
        }
        Ok(())
    }

    fn prelude<W: Write>(&mut self, sink: &mut LineSink<W>, rng: &mut Rng) -> io::Result<()> {
        self.turn_started_ms = self.clock_ms;
        let meta = json!({
            "session_id": self.id,
            "id": self.id,
            "timestamp": clock::iso8601(self.clock_ms),
            "cwd": self.cwd,
            "originator": self.originator,
            "cli_version": self.cli_version,
            "source": self.source,
            "thread_source": "user",
            "model_provider": "synthetic",
            "base_instructions": { "text": synth::blob(rng, "base instructions", 6) },
            "history_mode": "paginated",
            "context_window": { "window_id": rng.uuid(7) },
        });
        self.emit(sink, rng, "session_meta", meta)?;

        let started = self.task_started();
        self.emit(sink, rng, "event_msg", started)?;

        let instructions = synth::blob(rng, "developer instructions", 3);
        let item = self.message(rng, "developer", &instructions);
        self.emit(sink, rng, "response_item", item)?;

        let environment = format!(
            "<environment_context>\n  <cwd>{}</cwd>\n  <shell>zsh</shell>\n  \
             <current_date>{}</current_date>\n  <timezone>UTC</timezone>\n\
             </environment_context>",
            self.cwd,
            clock::date(self.clock_ms)
        );
        let item = self.message(rng, "user", &environment);
        self.emit(sink, rng, "response_item", item)?;

        let state = self.world_state();
        self.emit(sink, rng, "world_state", state)?;

        let context = self.turn_context();
        self.emit(sink, rng, "turn_context", context)
    }

    /// function_call → CommandExecution → function_call_output → token_count.
    fn tool_round<W: Write>(&mut self, sink: &mut LineSink<W>, rng: &mut Rng) -> io::Result<()> {
        self.call_seq += 1;
        let call_id = format!("call_synth_{}", rng.hex(16));
        let command = synth::shell_command(rng, &self.cwd);
        let output = synth::text(rng, "command output", self.call_seq, 5, 400);

        let item = self.response_item(
            json!({
                "type": "function_call",
                "id": format!("fc_{}", rng.uuid(7)),
                "name": "exec_command",
                // `arguments` is a JSON-encoded STRING, not an object.
                "arguments": serde_json::to_string(&json!({ "cmd": command }))
                    .map_err(io::Error::other)?,
                "call_id": call_id,
            }),
            false,
        );
        self.emit(sink, rng, "response_item", item)?;

        let execution = json!({
            "type": "CommandExecution",
            "id": call_id,
            "process_id": rng.range(1_000, 90_000).to_string(),
            "command": ["/bin/zsh", "-lc", command],
            "cwd": format!("file://{}", self.cwd),
            "parsed_cmd": [{ "type": "unknown", "cmd": command }],
            "source": "unified_exec_startup",
            "status": "completed",
            "stdout": output,
            "stderr": "",
            "aggregated_output": output,
            "exit_code": 0,
            "duration": { "secs": 0, "nanos": rng.range(1_000, 9_000_000) },
            "formatted_output": output,
        });
        let completed = self.item_completed(execution);
        self.emit(sink, rng, "event_msg", completed)?;

        let item = self.response_item(
            json!({
                "type": "function_call_output",
                "id": format!("fco_{}", rng.uuid(7)),
                "call_id": call_id,
                "output": format!(
                    "Chunk ID: {}\nWall time: 0.0100 seconds\nProcess exited with code 0\n\
                     Original token count: {}\nOutput:\n{}\n",
                    rng.hex(6),
                    output.len() / 4,
                    output
                ),
            }),
            true,
        );
        self.emit(sink, rng, "response_item", item)?;

        let delta = self.spend(rng);
        let event = self.token_count(rng, delta);
        self.emit(sink, rng, "event_msg", event)
    }

    /// AgentMessage → assistant message → token_count → task_complete.
    fn close_turn<W: Write>(&mut self, sink: &mut LineSink<W>, rng: &mut Rng) -> io::Result<()> {
        let reply = synth::text(rng, "assistant response", self.turn, 15, 300);
        self.last_agent_message = reply.clone();
        let message_id = format!("msg_{}", rng.uuid(7));

        let agent_item = json!({
            "type": "AgentMessage",
            "id": message_id,
            "content": [{ "type": "Text", "text": reply }],
        });
        let completed = self.item_completed(agent_item);
        self.emit(sink, rng, "event_msg", completed)?;

        // The response item and the completed thread item share one id — that
        // correspondence is how a consumer joins the two views of a reply.
        let item = self.response_item(
            json!({
                "type": "message",
                "id": message_id,
                "role": "assistant",
                "content": [{ "type": "output_text", "text": reply }],
            }),
            false,
        );
        self.emit(sink, rng, "response_item", item)?;

        let delta = self.spend(rng);
        let event = self.token_count(rng, delta);
        self.emit(sink, rng, "event_msg", event)?;

        let complete = self.task_complete();
        self.emit(sink, rng, "event_msg", complete)
    }

    /// Wrap a payload in the rollout envelope. `ordinal` is the line index —
    /// the sink already knows it.
    fn emit<W: Write>(
        &mut self,
        sink: &mut LineSink<W>,
        rng: &mut Rng,
        kind: &str,
        payload: Value,
    ) -> io::Result<()> {
        let line = json!({
            "timestamp": self.tick(rng),
            "ordinal": sink.lines(),
            "type": kind,
            "payload": payload,
        });
        sink.write(&line)
    }

    fn tick(&mut self, rng: &mut Rng) -> String {
        self.clock_ms += rng.range(120, 7_000) as i64;
        clock::iso8601(self.clock_ms)
    }

    /// Charge one model response to the session and return the delta.
    fn spend(&mut self, rng: &mut Rng) -> Tokens {
        let input = rng.range(400, 4_000);
        let delta = Tokens {
            input,
            cached: input / 2,
            cache_write: rng.below(400),
            output: rng.range(30, 1_200),
            reasoning: rng.below(500),
        };
        self.total.add(delta);
        self.last = delta;
        delta
    }

    /// Stamp the passthrough block every response item carries.
    fn response_item(&self, mut payload: Value, with_create_time: bool) -> Value {
        let mut passthrough = json!({ "turn_id": self.turn_id });
        if with_create_time {
            passthrough["create_time"] = json!(clock::epoch_secs_f64(self.clock_ms));
        }
        payload["internal_chat_message_metadata_passthrough"] = passthrough;
        payload
    }

    fn message(&self, rng: &mut Rng, role: &str, text: &str) -> Value {
        self.response_item(
            json!({
                "type": "message",
                "id": format!("msg_{}", rng.uuid(7)),
                "role": role,
                "content": [{
                    "type": if role == "assistant" { "output_text" } else { "input_text" },
                    "text": text,
                }],
            }),
            role != "developer",
        )
    }

    fn item_completed(&self, item: Value) -> Value {
        json!({
            "type": "item_completed",
            "thread_id": self.id,
            "turn_id": self.turn_id,
            "item": item,
            "started_at_ms": self.clock_ms,
            "completed_at_ms": self.clock_ms,
        })
    }

    fn task_started(&self) -> Value {
        json!({
            "type": "task_started",
            "turn_id": self.turn_id,
            "started_at": clock::epoch_secs(self.clock_ms),
            "model_context_window": CONTEXT_WINDOW,
            "collaboration_mode_kind": "default",
        })
    }

    fn task_complete(&self) -> Value {
        json!({
            "type": "task_complete",
            "turn_id": self.turn_id,
            "last_agent_message": self.last_agent_message,
            "started_at": clock::epoch_secs(self.turn_started_ms),
            "completed_at": clock::epoch_secs(self.clock_ms),
            "duration_ms": (self.clock_ms - self.turn_started_ms).max(0),
            "time_to_first_token_ms": 26,
        })
    }

    fn token_count(&mut self, rng: &mut Rng, delta: Tokens) -> Value {
        // A bare filler event still moves the meter, so cumulative totals never
        // stall or go backwards.
        let delta = if delta.total() == 0 {
            let bump = Tokens {
                input: rng.range(20, 200),
                cached: 0,
                cache_write: 0,
                output: rng.below(60),
                reasoning: 0,
            };
            self.total.add(bump);
            self.last = bump;
            bump
        } else {
            delta
        };
        json!({
            "type": "token_count",
            "info": {
                "total_token_usage": self.total.value(),
                "last_token_usage": delta.value(),
                "model_context_window": CONTEXT_WINDOW,
            },
            "rate_limits": {
                "limit_id": "codex",
                "limit_name": Value::Null,
                "primary": Value::Null,
                "secondary": Value::Null,
                "credits": Value::Null,
                "individual_limit": Value::Null,
                "spend_control_reached": Value::Null,
                "plan_type": Value::Null,
                "rate_limit_reached_type": Value::Null,
            },
        })
    }

    fn turn_context(&self) -> Value {
        json!({
            "turn_id": self.turn_id,
            "cwd": self.cwd,
            "workspace_roots": [self.cwd],
            "current_date": clock::date(self.clock_ms),
            "timezone": "UTC",
            "approval_policy": self.approval_policy,
            "approvals_reviewer": "user",
            "sandbox_policy": { "type": "danger-full-access" },
            "permission_profile": { "type": "disabled" },
            "model": self.model,
            "personality": self.personality,
            "collaboration_mode": {
                "mode": "default",
                "settings": {
                    "model": self.model,
                    "reasoning_effort": Value::Null,
                    "developer_instructions": Value::Null,
                },
            },
            "multi_agent_version": "v1",
            "realtime_active": false,
            "summary": "auto",
        })
    }

    fn world_state(&self) -> Value {
        json!({
            "full": true,
            "state": {
                "agents_md": {},
                "apps_instructions": false,
                "collaboration_mode": { "mode": "default", "model": self.model },
                "environments": {
                    "environments": {
                        "local": { "cwd": self.cwd, "status": "available", "shell": "zsh" },
                    },
                    "current_date": clock::date(self.clock_ms),
                    "timezone": "UTC",
                },
                "environments_instructions": false,
                "git_attribution": false,
            },
        })
    }
}

fn user_message_item(rng: &mut Rng, text: &str) -> Value {
    json!({
        "type": "UserMessage",
        "id": rng.uuid(7),
        "content": [{ "type": "text", "text": text, "text_elements": [] }],
    })
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
        for budget in [12u64, 13, 17, 19, 24, 100, 501] {
            for seed in [1u64, 2, 3] {
                assert_eq!(session_lines(budget, seed, 0.3).len() as u64, budget);
            }
        }
    }

    #[test]
    fn opens_on_the_session_prelude() {
        let lines = session_lines(60, 5, 0.5);
        let kinds: Vec<&str> = lines
            .iter()
            .take(6)
            .map(|line| line["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "session_meta",
                "event_msg",
                "response_item",
                "response_item",
                "world_state",
                "turn_context",
            ]
        );
        assert_eq!(lines[1]["payload"]["type"], "task_started");
        assert_eq!(lines[2]["payload"]["role"], "developer");
        assert_eq!(lines[0]["payload"]["id"], lines[0]["payload"]["session_id"]);
    }

    #[test]
    fn ordinals_are_the_line_index() {
        let lines = session_lines(120, 8, 0.4);
        for (index, line) in lines.iter().enumerate() {
            assert_eq!(line["ordinal"], index);
        }
    }

    #[test]
    fn token_totals_are_cumulative_and_never_regress() {
        let lines = session_lines(400, 13, 0.6);
        let mut previous = 0u64;
        let mut seen = 0;
        for line in &lines {
            if line["payload"]["type"] != "token_count" {
                continue;
            }
            let info = &line["payload"]["info"];
            let total = info["total_token_usage"]["total_tokens"].as_u64().unwrap();
            let last = info["last_token_usage"]["total_tokens"].as_u64().unwrap();
            assert!(total > previous, "cumulative totals must rise");
            assert!(last <= total);
            assert_eq!(total - previous, last, "the delta must be last_token_usage");
            previous = total;
            seen += 1;
        }
        assert!(seen > 5);
    }

    #[test]
    fn every_call_has_its_output_and_execution_item() {
        let lines = session_lines(400, 21, 1.0);
        let mut calls = Vec::new();
        let mut outputs = Vec::new();
        let mut executions = Vec::new();
        for line in &lines {
            let payload = &line["payload"];
            match payload["type"].as_str() {
                Some("function_call") => calls.push(payload["call_id"].clone()),
                Some("function_call_output") => outputs.push(payload["call_id"].clone()),
                Some("item_completed") if payload["item"]["type"] == "CommandExecution" => {
                    executions.push(payload["item"]["id"].clone())
                }
                _ => {}
            }
        }
        assert!(!calls.is_empty());
        assert_eq!(calls, outputs);
        assert_eq!(calls, executions);
        // The encoded arguments really are a JSON string, not an object.
        let arguments = lines
            .iter()
            .find_map(|line| line["payload"]["arguments"].as_str())
            .unwrap();
        assert!(serde_json::from_str::<Value>(arguments).unwrap()["cmd"].is_string());
    }

    #[test]
    fn turns_are_bracketed_by_started_and_complete() {
        let lines = session_lines(200, 30, 0.2);
        let mut depth = 0i32;
        for line in &lines {
            match line["payload"]["type"].as_str() {
                Some("task_started") => depth += 1,
                Some("task_complete") => depth -= 1,
                _ => {}
            }
            assert!((0..=1).contains(&depth), "task events must nest");
        }
        assert_eq!(depth, 0, "every turn closes");
    }

    #[test]
    fn the_tool_ratio_is_honoured() {
        assert_eq!(function_calls(&session_lines(600, 33, 0.0)), 0);
        assert!(function_calls(&session_lines(600, 33, 1.0)) > 10);
    }

    fn function_calls(lines: &[Value]) -> usize {
        lines
            .iter()
            .filter(|line| line["payload"]["type"] == "function_call")
            .count()
    }
}
