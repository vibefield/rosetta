//! The R1 brief §10 fixtures, generated into each test's own temporary home
//! in the shapes the recorded ground truth pins (Claude Code 2.1.241
//! transcripts, subagent sidecars and meta documents; Codex rollouts and its
//! state database). Never committed raw data; never a live home.
//!
//! File times are set explicitly: a root tree's recency is its newest
//! source's mtime, so these decide which tree each walk reaches first.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::clock::iso8601;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_pipeline::{IngestOptions, SessionFile, expand_data_root};
use serde_json::{Value, json};
use tempfile::TempDir;

/// 2026-08-29T08:00:00Z: every generated record is stamped after it.
pub const T0: i64 = 1_788_336_000_000;
const HOUR: i64 = 3_600_000;

pub fn claude_root(home: &Path) -> PathBuf {
    expand_data_root(&ClaudeCodeAdapter.layout().data_root, home)
}

pub fn codex_root(home: &Path) -> PathBuf {
    expand_data_root(&CodexAdapter.layout().data_root, home)
}

/// Set a file's mtime to `ms` after the epoch.
pub fn set_mtime(path: &Path, ms: i64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_millis(ms as u64))
        .unwrap();
}

pub fn write_lines(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(path, text).unwrap();
}

/// A recorded-shape Claude Code usage block; `iterations` holds the extra
/// model calls of one response (advisor, compaction).
pub fn claude_usage(input: u64, output: u64, iterations: Value) -> Value {
    json!({
        "input_tokens": input, "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0, "output_tokens": output,
        "output_tokens_details": {"thinking_tokens": 0},
        "server_tool_use": {"web_search_requests": 0, "web_fetch_requests": 0},
        "service_tier": "standard",
        "cache_creation": {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0},
        "inference_geo": "", "iterations": iterations, "speed": "standard",
    })
}

/// One Claude Code transcript (a main session's, or a subagent's when
/// `agent` is set), record by record, its `parentUuid` chained.
pub struct ClaudeTranscript {
    session: String,
    agent: Option<String>,
    cwd: String,
    ms: i64,
    parent: Option<String>,
    count: u64,
    lines: Vec<Value>,
}

impl ClaudeTranscript {
    pub fn new(session: &str, agent: Option<&str>, start_ms: i64) -> Self {
        Self {
            session: session.to_owned(),
            agent: agent.map(str::to_owned),
            cwd: "/w/project".to_owned(),
            ms: start_ms,
            parent: None,
            count: 0,
            lines: Vec::new(),
        }
    }

    fn envelope(&mut self, kind: &str, message: Value) -> Value {
        self.count += 1;
        self.ms += 1_000;
        let prefix = self.agent.as_deref().unwrap_or(&self.session);
        let uuid = format!("{prefix}-{:04}", self.count);
        let mut record = json!({
            "parentUuid": self.parent, "isSidechain": self.agent.is_some(),
            "type": kind, "message": message, "uuid": uuid,
            "timestamp": iso8601(self.ms), "userType": "external", "entrypoint": "sdk-cli",
            "cwd": self.cwd, "sessionId": self.session, "version": "2.1.241",
            "gitBranch": "main",
        });
        if let Some(agent) = &self.agent {
            record["agentId"] = json!(agent);
        }
        self.parent = Some(uuid);
        record
    }

    pub fn prompt(&mut self, text: &str) -> &mut Self {
        let record = self.envelope("user", json!({"role": "user", "content": text}));
        self.lines.push(record);
        self
    }

    /// A reply carrying `usage` under the response identity `message_id`
    /// and `request_id`.
    pub fn reply_as(
        &mut self,
        message_id: &str,
        request_id: &str,
        text: &str,
        usage: Value,
    ) -> &mut Self {
        let mut record = self.envelope(
            "assistant",
            json!({
                "content": [{"text": text, "type": "text"}], "id": message_id,
                "model": "claude-opus-5", "role": "assistant", "stop_reason": "end_turn",
                "stop_sequence": null, "type": "message", "usage": usage,
            }),
        );
        record["requestId"] = json!(request_id);
        self.lines.push(record);
        self
    }

    pub fn reply(&mut self, text: &str) -> &mut Self {
        let id = format!("msg_{}_{:04}", self.prefix(), self.count + 1);
        let request = format!("req_{}_{:04}", self.prefix(), self.count + 1);
        self.reply_as(&id, &request, text, claude_usage(128, 32, json!([])))
    }

    fn prefix(&self) -> String {
        self.agent.clone().unwrap_or_else(|| self.session.clone())
    }

    /// A Bash call and its result, printing `output`.
    pub fn tool(&mut self, command: &str, output: &str) -> &mut Self {
        let tool_id = format!("toolu_{}_{:04}", self.prefix(), self.count + 1);
        let message_id = format!("msg_{}_{:04}", self.prefix(), self.count + 1);
        let mut call = self.envelope(
            "assistant",
            json!({
                "content": [{"id": tool_id, "input": {"command": command}, "name": "Bash",
                             "type": "tool_use"}],
                "id": message_id, "model": "claude-opus-5", "role": "assistant",
                "stop_reason": "tool_use", "stop_sequence": null, "type": "message",
                "usage": claude_usage(64, 16, json!([])),
            }),
        );
        call["requestId"] = json!(format!("req_{}_{:04}", self.prefix(), self.count));
        let call_uuid = call["uuid"].clone();
        self.lines.push(call);
        let mut result = self.envelope(
            "user",
            json!({"role": "user", "content": [{
                "tool_use_id": tool_id, "type": "tool_result", "content": output,
                "is_error": false,
            }]}),
        );
        result["toolUseResult"] = json!({
            "stdout": output, "stderr": "", "interrupted": false, "isImage": false,
            "noOutputExpected": false,
        });
        result["sourceToolAssistantUUID"] = call_uuid;
        self.lines.push(result);
        self
    }

    /// `count` prompt/reply turns of about `bytes` each, then `count` tool
    /// calls printing about `bytes` each.
    pub fn bulk(&mut self, count: usize, bytes: usize) -> &mut Self {
        for index in 0..count {
            self.prompt(&filler("ask", index, bytes / 2));
            self.reply(&filler("answer", index, bytes / 2));
        }
        self
    }

    /// `count` tool calls whose commands are about `bytes` long (a tool
    /// row keeps its command; its output stays in the raw ledger).
    pub fn tools(&mut self, count: usize, bytes: usize) -> &mut Self {
        for index in 0..count {
            let command = format!("echo {}", filler("step", index, bytes));
            self.tool(&command, &format!("step {index} done"));
        }
        self
    }

    /// Write the transcript and stamp its mtime.
    pub fn write(&self, path: &Path, mtime_ms: i64) {
        write_lines(path, &self.lines);
        set_mtime(path, mtime_ms);
    }
}

/// Deterministic prose of about `bytes`.
pub fn filler(label: &str, index: usize, bytes: usize) -> String {
    let word = format!("{label}{index} lorem ipsum dolor sit amet ");
    word.repeat(bytes / word.len() + 1)[..bytes.max(1)].to_owned()
}

/// A Claude subagent's meta document.
pub fn claude_meta(path: &Path, parent_agent: Option<&str>) {
    let mut meta = json!({"agentType": "Explore", "description": "delegated work"});
    if let Some(parent) = parent_agent {
        meta["parentAgentId"] = json!(parent);
    }
    std::fs::write(path, meta.to_string()).unwrap();
}

/// Main session `m` with subagent `a` — a scaled giant of `giant` bytes —
/// and its meta document, subagent `c` nested under `a`, a sidecar-only
/// subagent `z` whose parent agent `zz` exists only as a placeholder; and a
/// separate small main session `n`, older than `m`'s tree.
pub fn claude_nested(home: &Path, giant: usize) {
    let project = claude_root(home).join("projects/-w-project");
    let mut main = ClaudeTranscript::new("m", None, T0);
    main.prompt("start the work")
        .reply("delegating")
        .tool("ls", "a b c");
    main.write(&project.join("m.jsonl"), T0 + 10 * HOUR);
    let subagents = project.join("m/subagents");
    let mut a = ClaudeTranscript::new("m", Some("a"), T0 + 60_000);
    a.prompt("delegated work")
        .bulk(giant / 4096, 2048)
        .tools(4, 512);
    a.write(&subagents.join("agent-a.jsonl"), T0 + 10 * HOUR + 1);
    let mut c = ClaudeTranscript::new("m", Some("c"), T0 + 120_000);
    c.prompt("nested work").reply("done nested");
    c.write(&subagents.join("agent-c.jsonl"), T0 + 10 * HOUR + 2);
    for (agent, parent) in [("a", None), ("c", Some("a")), ("z", Some("zz"))] {
        claude_meta(&subagents.join(format!("agent-{agent}.meta.json")), parent);
    }
    let mut n = ClaudeTranscript::new("n", None, T0);
    n.prompt("another session")
        .reply("fine")
        .tool("pwd", "/w/project");
    n.write(&project.join("n.jsonl"), T0 + HOUR);
}

/// An origin session and a fork that copies its first response, in two
/// separate trees. The copied response carries advisor and compaction
/// iterations, so its usage is a row plus iteration samples under one
/// response identity. The fork's file is the newer: the recency walk lands
/// the copy first, the reversed walk the origin first.
pub fn claude_fork(home: &Path) {
    let project = claude_root(home).join("projects/-w-fork");
    let iterations = json!([
        {"type": "message", "input_tokens": 300, "output_tokens": 40},
        {"type": "advisor_message", "model": "claude-haiku-4-5",
         "input_tokens": 90, "output_tokens": 12},
        {"type": "compaction", "input_tokens": 900, "output_tokens": 60},
    ]);
    let usage = claude_usage(300, 40, iterations);
    let mut origin = ClaudeTranscript::new("origin", None, T0);
    origin
        .prompt("plan the change")
        .reply_as(
            "msg_fork_1",
            "req_fork_1",
            "here is the plan",
            usage.clone(),
        )
        .tool("git status", "clean")
        .prompt("go on")
        .reply("going on");
    origin.write(&project.join("origin.jsonl"), T0 + 2 * HOUR);
    // The fork, opened an hour later, restates the origin's opening
    // exchange under its own id; the origin owns the response (the earlier
    // session).
    let mut fork = ClaudeTranscript::new("fork", None, T0 + HOUR);
    fork.prompt("plan the change")
        .reply_as("msg_fork_1", "req_fork_1", "here is the plan", usage)
        .prompt("try another way")
        .reply("another way")
        .tool("git diff", "+one line");
    fork.write(&project.join("fork.jsonl"), T0 + 5 * HOUR);
}

/// A generated Claude Code history of `sessions` sessions with the newest
/// tree a giant main session of about `giant` bytes: `giant / 2` of turns
/// and `giant / 2` of tool output.
pub fn giant_newest(home: &Path, sessions: u64, giant: usize, seed: u64) {
    let root = claude_root(home);
    std::fs::create_dir_all(&root).unwrap();
    generate(
        &CorpusSpec::new(Agent::ClaudeCode, sessions, sessions * 120, seed),
        &root,
    )
    .unwrap();
    for (index, file) in super::harness::walkdir(&root).iter().enumerate() {
        set_mtime(file, T0 + index as i64 * HOUR);
    }
    let mut big = ClaudeTranscript::new("giant", None, T0);
    big.bulk(giant / 2 / 4096, 4096)
        .tools(giant / 2 / 8192, 8192);
    big.write(
        &root.join("projects/-w-giant/giant.jsonl"),
        T0 + 1_000 * HOUR,
    );
}

fn codex_record(timestamp: &str, ordinal: u64, kind: &str, payload: Value) -> Value {
    json!({"timestamp": timestamp, "ordinal": ordinal, "type": kind, "payload": payload})
}

fn codex_message(ordinal: u64, role: &str, text: &str) -> Value {
    codex_record(
        "2026-09-20T10:00:01.000Z",
        ordinal,
        "response_item",
        json!({
            "type": "message", "role": role,
            "content": [{"type": if role == "user" { "input_text" } else { "output_text" },
                         "text": text}],
        }),
    )
}

/// An `exec_command` call at `ordinal` and its output right after.
fn codex_tool_call(ordinal: u64, call_id: &str, cmd: &str) -> [Value; 2] {
    [
        codex_record(
            "2026-09-20T10:00:02.000Z",
            ordinal,
            "response_item",
            json!({"type": "function_call", "id": format!("fc_{call_id}"),
                   "name": "exec_command",
                   "arguments": json!({"cmd": cmd}).to_string(), "call_id": call_id}),
        ),
        codex_record(
            "2026-09-20T10:00:02.500Z",
            ordinal + 1,
            "response_item",
            json!({"type": "function_call_output", "id": format!("fco_{call_id}"),
                   "call_id": call_id,
                   "output": "Process exited with code 0\nOutput:\nreviewed\n"}),
        ),
    ]
}

fn codex_header(id: &str, extra: Value) -> Value {
    let mut meta = json!({
        "id": id, "timestamp": "2026-09-20T10:00:00.100Z", "cwd": "/w/project",
        "originator": "codex_exec", "cli_version": "0.153.4", "source": "exec",
        "model_provider": "openai", "history_mode": "paginated",
    });
    meta.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    codex_record("2026-09-20T10:00:00.100Z", 0, "session_meta", meta)
}

fn codex_usage(input: i64, output: i64) -> Value {
    json!({
        "input_tokens": input, "cached_input_tokens": 0, "cache_write_input_tokens": 0,
        "output_tokens": output, "reasoning_output_tokens": 0, "total_tokens": input + output,
    })
}

/// Root `r`, child `c` and grandchild `g` (each child's root its immediate
/// parent, as Codex writes it), a guardian `x` reviewing `g`, an unrelated
/// root `u`, `s` that exists only as a state-database row, and children
/// `k1`, `k2` of `p0`, a root no source holds (a metadata-only placeholder).
pub fn codex_nested(home: &Path) {
    let root = codex_root(home);
    let rollout = |id: &str| root.join(format!("sessions/2026/09/20/rollout-{id}.jsonl"));
    let family = [
        ("r", None, 9),
        ("c", Some("r"), 9),
        ("g", Some("c"), 9),
        ("u", None, 4),
        ("k1", Some("p0"), 6),
        ("k2", Some("p0"), 6),
    ];
    for (id, parent, hours) in family {
        let extra = match parent {
            Some(parent) => json!({"parent_thread_id": parent, "thread_source": "subagent",
                "source": {"subagent": {"thread_spawn": {"parent_thread_id": parent}}}}),
            None => json!({}),
        };
        let path = rollout(id);
        let mut lines = vec![
            codex_header(id, extra),
            codex_message(1, "user", &format!("do the work of {id}")),
        ];
        if id == "g" {
            // The guardian's samples cross `g`, so this tool row is left to
            // the fold its tree's flip writes.
            lines.extend(codex_tool_call(2, "call_g_1", "echo reviewed"));
        }
        lines.push(codex_message(4, "assistant", "done"));
        write_lines(&path, &lines);
        set_mtime(&path, T0 + hours * HOUR);
    }
    let guardian = rollout("x");
    write_lines(
        &guardian,
        &[
            codex_header(
                "x",
                json!({"parent_thread_id": "g", "thread_source": "guardian_review",
                    "source": {"subagent": {"other": "guardian"}},
                    "subagent_history_start_ordinal": 30}),
            ),
            codex_record(
                "2026-09-20T10:00:00.200Z",
                1,
                "turn_context",
                json!({"cwd": "/w/project", "model": "codex-auto-review"}),
            ),
            codex_record(
                "2026-09-20T10:00:05.000Z",
                2,
                "event_msg",
                json!({"type": "token_count", "info": {
                    "total_token_usage": codex_usage(300, 30),
                    "last_token_usage": codex_usage(300, 30),
                }}),
            ),
        ],
    );
    set_mtime(&guardian, T0 + 9 * HOUR);
    let state_path = root.join("state_5.sqlite");
    let state = rusqlite::Connection::open(&state_path).unwrap();
    state
        .execute_batch(
            "CREATE TABLE threads(id TEXT PRIMARY KEY, source TEXT, thread_source TEXT, \
             cwd TEXT, title TEXT, cli_version TEXT, created_at INTEGER, updated_at INTEGER);",
        )
        .unwrap();
    let spawn = |parent: &str| json!({"subagent": {"thread_spawn": {"parent_thread_id": parent}}});
    for (id, source, thread_source) in [
        ("r", json!("exec"), "user"),
        ("c", spawn("r"), "subagent"),
        ("g", spawn("c"), "subagent"),
        (
            "x",
            json!({"subagent": {"other": "guardian"}}),
            "guardian_review",
        ),
        ("u", json!("exec"), "user"),
        ("s", json!("exec"), "user"),
    ] {
        let source = source
            .as_str()
            .map_or_else(|| source.to_string(), str::to_owned);
        state
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, '/w/project', 'Work', '0.153.4', 1, 2)",
                rusqlite::params![id, source, thread_source],
            )
            .unwrap();
    }
    drop(state);
    set_mtime(&state_path, T0 + 2 * HOUR);
}

/// Rewrites one source the first time capture reports bytes for it — after
/// its first batch was framed and before the next read — so the tailer sees
/// a changed anchor and restarts the generation mid-pass.
#[derive(Debug)]
pub struct RewriteOnce {
    root: PathBuf,
    target: String,
    replacement: String,
    done: AtomicBool,
}

impl rosetta_pipeline::progress::ColdProgressObserver for RewriteOnce {
    fn source_bytes(&self, file: &SessionFile, _bytes: u64) {
        if file.relative_path == self.target && !self.done.swap(true, Ordering::SeqCst) {
            std::fs::write(self.root.join(&self.target), &self.replacement).unwrap();
        }
    }

    fn reading_finished(&self) {}
}

pub fn claude_user(uuid: &str, content: &str) -> String {
    json!({
        "parentUuid": null,
        "isSidechain": false,
        "type": "user",
        "message": {"role": "user", "content": content},
        "uuid": uuid,
        "timestamp": "2026-08-25T02:35:19.054Z",
        "userType": "external",
        "cwd": "/w/heal",
        "sessionId": "heal-session",
        "version": "2.1.241",
        "gitBranch": "main"
    })
    .to_string()
}

pub const HEAL_TRANSCRIPT: &str = "projects/-w-heal/heal-session.jsonl";
pub const HEAL_JOURNAL: &str =
    "projects/-w-heal/heal-session/subagents/workflows/wf_heal/journal.jsonl";

/// A two-transcript session (transcript + workflow journal) whose transcript
/// is rewritten between its first and second capture batch.
pub fn restarting_home() -> (TempDir, IngestOptions) {
    let home = TempDir::new().unwrap();
    let root = claude_root(home.path());
    let lines = |prefix: &str| {
        (1..=5)
            .map(|index| {
                claude_user(
                    &format!("{prefix}-{index}"),
                    &format!("{prefix} line {index}"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    let transcript = root.join(HEAL_TRANSCRIPT);
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, lines("old")).unwrap();
    let journal = root.join(HEAL_JOURNAL);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        [
            r#"{"type":"started","key":"worker-1","agentId":"agent-1"}"#,
            r#"{"type":"result","key":"worker-1","result":{"ok":true}}"#,
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let mut tail = rosetta_capture::TailConfig::json_lines();
    tail.max_records_per_batch = 2;
    let opts = IngestOptions {
        tail,
        progress: Some(Arc::new(RewriteOnce {
            root,
            target: HEAL_TRANSCRIPT.into(),
            replacement: lines("new"),
            done: AtomicBool::new(false),
        })),
        ..IngestOptions::default()
    };
    (home, opts)
}

/// One generated multi-session corpus of `agent` in `home`.
pub fn generated(home: &Path, adapter: &dyn AgentAdapter, agent: Agent, seed: u64) {
    let root = expand_data_root(&adapter.layout().data_root, home);
    std::fs::create_dir_all(&root).unwrap();
    generate(&CorpusSpec::new(agent, 12, 1_500, seed), &root).unwrap();
}

/// Three Claude Code trees in lane S, newest first: `alpha` (small, flips
/// first), `bravo` (about `immediate` bytes of turns and `tools` tool calls,
/// whose rows the writer resolves: a source it steps in Stage and
/// resolved-row slices) and `delta` (small, walked last).
pub fn flipped_beside_sliced(home: &Path, immediate: usize, tools: usize) {
    let projects = claude_root(home).join("projects");
    let mut alpha = ClaudeTranscript::new("alpha", None, T0);
    alpha
        .prompt("first question")
        .reply("first answer")
        .tool("ls", "a b c");
    alpha.write(&projects.join("-w-alpha/alpha.jsonl"), T0 + 10 * HOUR);
    let mut bravo = ClaudeTranscript::new("bravo", None, T0);
    bravo.bulk(immediate / 8192, 8192).tools(tools, 64);
    bravo.write(&projects.join("-w-bravo/bravo.jsonl"), T0 + 5 * HOUR);
    let mut delta = ClaudeTranscript::new("delta", None, T0);
    delta.prompt("later").reply("sure");
    delta.write(&projects.join("-w-delta/delta.jsonl"), T0 + HOUR);
}
