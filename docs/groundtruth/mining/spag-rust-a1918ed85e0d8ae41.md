I'll read the primary file fully, then the supporting files.
Continuing the primary file, plus the engine projections.
# Claude Code subagent-transcript discovery & parsing in `spaghetti-napi`

All paths are relative to `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/`. Absolute paths are given at first mention.

---

## 0. Orientation — there are TWO parallel ingest paths in this crate

You have to hold both in your head, because they answer the same questions differently and both are live:

| | **Path A — legacy stream parser** | **Path B — adapter / ADS** |
|---|---|---|
| Entry | `ProjectParser::parse_project` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/project_parser.rs:116` | `ClaudeCodeAdapter::streams` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter.rs:328` |
| Discovery | `std::fs::read_dir` walks, hand-rolled per artifact type | Declarative `ObjectSelector { root_name, include: [glob] }` |
| Output | `IngestEvent` over a `crossbeam_channel` → `core/writer.rs` → tables `subagents`, `subagent_messages`, `workflows` | `Fact::*` → `engine/*_projection.rs` → `canonical_*` tables |
| Warm start | `claude/fingerprint.rs` (`source_files` categories) | catalog + cursors (`claude/catalog_discovery.rs`) |
| Ported from | `packages/sdk/src/parser/project-parser.ts` (comments say so verbatim) | RFC 012A/012C native fact model |

Path A is the one your PRIMARY FILE implements. Path B's globs are the authoritative "glob strings" answer.

---

## 1. Path patterns, globs, and ID extraction

### 1.1 The declarative globs (Path B — `claude/adapter.rs`)

These are the literal strings. Root `"projects"` = `<claude_home>/projects`.

```rust
// adapter.rs:347-357  — SUBAGENT_STREAM
selector: ObjectSelector {
    root_name: "projects".to_string(),
    include: vec![
        "*/*/subagents/**/agent-*.jsonl".to_string(),
        "*/*/subagents/workflows/*/**/agent-*.jsonl".to_string(),
    ],
    exclude: Vec::new(),
},
```

```rust
// adapter.rs:367-376 — SUBAGENT_META_STREAM
include: vec!["*/*/subagents/**/agent-*.meta.json".to_string()],
```

```rust
// adapter.rs:539-546 — WORKFLOW_JOURNAL_STREAM
include: vec!["*/*/subagents/workflows/*/journal.jsonl".to_string()],
```

Full glob table (`adapter.rs`):

| Stream const | line | root | include glob(s) |
|---|---|---|---|
| `PARENT_STREAM` (`session-transcripts`) | 335 | projects | `*/*.jsonl` |
| `SUBAGENT_STREAM` | 353-354 | projects | `*/*/subagents/**/agent-*.jsonl`, `*/*/subagents/workflows/*/**/agent-*.jsonl` |
| `SUBAGENT_META_STREAM` | 374 | projects | `*/*/subagents/**/agent-*.meta.json` |
| `WORKFLOW_RUN_STREAM` | 527 | projects | `*/*/workflows/wf_*.json` |
| `WORKFLOW_JOURNAL_STREAM` | 544 | projects | `*/*/subagents/workflows/*/journal.jsonl` |
| `SESSION_INDEX_STREAM` | 563 | projects | `*/sessions-index.json` |
| `PROJECT_MEMORY_STREAM` | 582 | projects | `*/memory/*.md` |
| `PERSISTED_TOOL_RESULT_STREAM` | 601 | projects | `*/*/tool-results/*.txt` |
| `TODO_STREAM` | 451 | home | `todos/*-agent-*.json` |
| `TASK_ITEM_STREAM` | 470 | home | `tasks/*/*.json` |
| `PLAN_STREAM` | 489 | home | `plans/*.md` |
| `ARTIFACT_CONTENT_STREAM` | 508 | home | `file-history/*/*@v*` |
| `TEAM_CONFIG_STREAM` | 393 | teams | `*/config.json` |
| `TEAM_INBOX_STREAM` | 412 | teams | `*/inboxes/*.json` |
| `ACTIVE_SESSION_STREAM` | 432 | sessions | `*.json` |
| `INTERPRETATION_SETTINGS_STREAM` | 620-623 | home | `settings.json`, `settings.local.json` |

Roots are declared at `adapter.rs:296-313`: `home` = `~/.claude`, `projects` = `home/projects`, `teams` = `home/teams`, `sessions` = `home/sessions`.

### 1.2 The imperative walks (Path A — `project_parser.rs`)

`read_subagents` does **two** distinct walks and they are not symmetric:

```rust
// project_parser.rs:647-698
fn read_subagents(
    project_dir: &Path,
    session_id: &str,
) -> Vec<(SubagentTranscript, Option<DroppedSubagentLines>)> {
    let dir = project_dir.join(session_id).join("subagents");
    let mut out = Vec::new();

    // Top-level subagent transcripts (not associated with a workflow).
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            ...
            if !name.ends_with(".jsonl") {        // <-- ANY *.jsonl, no agent- prefix required
                continue;
            }
            out.push(read_one_subagent(&entry.path(), name, ""));
        }
    }

    // Nested workflow subagent transcripts:
    //   subagents/workflows/{wf_id}/agent-*.jsonl  (journal.jsonl is skipped
    //   by the `agent-` prefix). ...
    if let Ok(wf_dirs) = std::fs::read_dir(dir.join("workflows")) {
        for wf_entry in wf_dirs.flatten() {
            if !wf_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
            let wf_name = wf_entry.file_name();
            let Some(workflow_id) = wf_name.to_str() else { continue; };
            if let Ok(agent_dir) = std::fs::read_dir(wf_entry.path()) {
                for entry in agent_dir.flatten() {
                    ...
                    if !name.starts_with("agent-") || !name.ends_with(".jsonl") { continue; }
                    out.push(read_one_subagent(&entry.path(), name, workflow_id));
                }
            }
        }
    }
    out
}
```

Notable asymmetries:
- **Top-level** (`project_parser.rs:661`): accepts **any** `*.jsonl` in `subagents/`, not just `agent-*`.
- **Nested** (`project_parser.rs:688`): requires the `agent-` prefix, which is what excludes `journal.jsonl`.
- **Nested walk is only one level deep** — `subagents/workflows/<wf>/agent-*.jsonl` only. The adapter's glob `*/*/subagents/workflows/*/**/agent-*.jsonl` allows deeper nesting (e.g. `.../wf_main/agents/agent-child.jsonl`, exercised at `adapter.rs:4058` and `adapter.rs:5852`). **Path A would miss those; Path B catches them.** Same for `**` in the flat glob.
- `workflow_id` is literally the directory name under `workflows/` — no validation in Path A (Path B requires `wf_` prefix, see §4).

### 1.3 `agent-{id}.meta.json` sidecar

Path A reads it by suffix substitution, not by glob:

```rust
// project_parser.rs:746-749
// Sibling `agent-{id}.meta.json` carries the real agent type + description.
let meta = std::fs::read_to_string(path.with_extension("meta.json"))
    .ok()
    .and_then(|raw| serde_json::from_str::<SubagentMeta>(&raw).ok());
```

`Path::with_extension` replaces only the final extension, so `agent-a1.jsonl` → `agent-a1.meta.json`. A malformed or missing sidecar is silently `None` (no `RecordSkip`).

`SubagentMeta` shape — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/project.rs:93-106`:

```rust
pub struct SubagentMeta {
    pub agent_type: String,
    // Absent on nested workflow metas — must be optional or their parse fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub name: Option<String>,
    pub spawn_depth: Option<f64>,
    pub worktree_path: Option<String>,
}
```

Path B's richer version adds `teamName` and `toolUseId` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter/sidecars.rs:13-29`:

```rust
pub(super) struct ClaudeSubagentMetadataDocument {
    pub(super) agent_type: String,
    pub(super) description: Option<String>,
    pub(super) name: Option<String>,
    pub(super) team_name: Option<String>,
    pub(super) spawn_depth: Option<u32>,
    pub(super) worktree_path: Option<String>,
    pub(super) tool_use_id: Option<String>,
}
```

A real fixture is at `adapter.rs:7045`:
```
{"agentType":"Explore","description":"map the parser","name":"survey","spawnDepth":2,"worktreePath":"/repo/.worktrees/a1","toolUseId":"tool-7"}
```

### 1.4 `agentId` extraction from the filename

**Path A** — `project_parser.rs:49-50` and `860-868`:

```rust
/// Matches `agent-<id>.jsonl` where `<id>` starts with `a`. Port of
/// `extractAgentId`'s `^agent-(a.+)\.jsonl$` pattern.
static SUBAGENT_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^agent-(a.+)\.jsonl$").expect("SUBAGENT_FILE regex compiles"));
```

```rust
/// TS: `const match = fileName.match(/^agent-(a.+)\.jsonl$/);`
fn extract_agent_id(file_name: &str) -> String {
    if let Some(caps) = SUBAGENT_FILE.captures(file_name) {
        caps.get(1)
            .map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| file_name.trim_end_matches(".jsonl").to_owned())
    } else {
        file_name.trim_end_matches(".jsonl").to_owned()
    }
}
```

The `a` inside `(a.+)` is **part of the capture group**, so `agent-abc123.jsonl` → `"abc123"` (asserted at `project_parser.rs:1435`). But the id **must start with `a`** or the regex fails and the fallback yields the **whole stem including the `agent-` prefix**: `agent-xyz.jsonl` → `"agent-xyz"`.

**This is a real cross-path divergence.** Compare:

- `claude/fingerprint.rs:93-94`: `^agent-(.+)\.jsonl$` — no `a` constraint.
- `adapter.rs:1071-1077`: pure `strip_prefix`/`strip_suffix`, no `a` constraint:

```rust
let Some(agent_id) = file_name
    .strip_prefix("agent-")
    .and_then(|name| name.strip_suffix(suffix))
    .filter(|value| !value.is_empty())
else {
    return Err(path_error(relative_path, "invalid subagent object name"));
};
```

So for a hypothetical `agent-b7.jsonl`, Path A stores `agent_id = "agent-b7"` and Path B stores `"b7"`.

### 1.5 `sessionId` / `workflowId` extraction from directories

**Path A**: `session_id` is never parsed out of a subagent path — it is passed down from the enclosing session loop (`project_parser.rs:382`, `read_subagents(project_dir, &session_id)`). `workflow_id` is the raw directory name (`project_parser.rs:678-680`).

**Path B** — `adapter.rs:1062-1087`, `ClaudeTranscriptContext::subagent_object`:

```rust
fn subagent_object(relative_path: &Path, suffix: &str) -> Result<Self, AdapterError> {
    let components = utf8_components(relative_path)?;
    if components.len() < 4 || components.get(2).map(String::as_str) != Some("subagents") {
        return Err(path_error(relative_path, "subagent object path does not match Claude layout"));
    }
    let file_name = components.last().expect("minimum component count checked");
    let Some(agent_id) = file_name
        .strip_prefix("agent-")
        .and_then(|name| name.strip_suffix(suffix))
        .filter(|value| !value.is_empty())
    else { return Err(path_error(relative_path, "invalid subagent object name")); };
    let workflow_id = components.windows(2).find_map(|pair| {
        (pair[0] == "workflows" && !pair[1].is_empty()).then(|| pair[1].clone())
    });
    Ok(Self {
        project_slug: components[0].clone(),
        session_id: components[1].clone(),
        agent_id: Some(agent_id.to_string()),
        workflow_id,
    })
}
```

So `components[0] = project_slug`, `components[1] = session_id`, `components[2]` must literally be `"subagents"`, and `workflow_id` is the component immediately after any component named `workflows` (found by sliding window, so it works at any depth).

Suffix dispatch: `.jsonl` for transcripts (`adapter.rs:1054-1056`), `.meta.json` for sidecars (`adapter.rs:1058-1060`).

Workflow journal path is parsed with a strict fixed shape — `adapter.rs:1534-1552`:

```rust
fn journal_from_path(relative_path: &Path) -> Result<Self, AdapterError> {
    let components = utf8_components(relative_path)?;
    if components.len() != 6
        || components[2] != "subagents"
        || components[3] != "workflows"
        || components[5] != "journal.jsonl"
    {
        return Err(path_error(relative_path,
            "workflow journal path must be <project>/<session>/subagents/workflows/<workflow>/journal.jsonl"));
    }
    Self::validate(relative_path, &components[0], &components[1], &components[4])
}
```

Workflow run doc path — `adapter.rs:1515-1532`: exactly 4 components, `components[2] == "workflows"`, `runId` = `components[3]` minus `.json`.

`validate` (`adapter.rs:1554-1580`) enforces: non-empty slug, `is_uuid(session_id)`, and `native_workflow_id.strip_prefix("wf_")` with a non-empty suffix.

### 1.6 `workflows/{runId}.json` in Path A

`project_parser.rs:764-821`:

```rust
if !name.starts_with("wf_") || !name.ends_with(".json") { continue; }   // :776
...
let workflow_id = data
    .get("runId")
    .and_then(|v| v.as_str())
    .map(str::to_owned)
    .unwrap_or_else(|| name.trim_end_matches(".json").to_owned());      // :786-790
```

So the workflow id comes from the **JSON body's `runId`**, with the filename stem as fallback. (Path B instead *rejects* the document when `runId != filename` — `sidecars.rs:994-1001`.)

`subagent_count` is computed by re-counting the directory (`project_parser.rs:802`, `count_workflow_subagents` at `:823-841`, which requires `agent-` prefix + `.jsonl`). Journal is read via `read_workflow_journal` (`:843-857`) at `<session>/subagents/workflows/<wf>/journal.jsonl` into `Vec<Value>`, silently dropping unparseable lines. Runs are sorted by `workflow_id` (`:819`).

### 1.7 Other artifact patterns in Path A

| Artifact | Location | Pattern | Code |
|---|---|---|---|
| session JSONL | `projects/<slug>/` | `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$` | `project_parser.rs:42-45` |
| tool results | `projects/<slug>/<sid>/tool-results/` | `*.txt`, `tool_use_id` = stem | `:888-913` |
| file history | `<root>/file-history/<sid>/` | `^([0-9a-f]+)@v(\d+)$` | `:54-56`, `:917-968` |
| todos | `<root>/todos/` | prefix `"{session_id}-agent-"` + `.json`, then `^(.+?)-agent-(.+)\.json$` | `:60-61`, `:972-1013` |
| tasks | `<root>/tasks/<sid>/.lock`, `.highwatermark` | existence + `parseInt`-lenient | `:1017-1040` |
| plans | `<root>/plans/*.md` | title = first `(?m)^#\s+(.+)$` | `:1222-1275` |
| memory | `projects/<slug>/memory/MEMORY.md` | fixed | `:640-643` |

Fingerprint categories (warm start) — `claude/fingerprint.rs:30-40` documents the same taxonomy; subagent-related constants at `fingerprint.rs:62-82`, with `CATEGORY_SUBAGENT_META` and `CATEGORY_WORKFLOW_JOURNAL` added specifically because "Discovery previously saw only the flat `agent-*.jsonl`, so a changed agent type, a changed journal, or a whole workflow's transcripts were invisible to warm start — RFC 008 Phase 1.4" (`fingerprint.rs:608-616`). The fingerprint walk mirrors the parser at `fingerprint.rs:617-712`.

---

## 2. How subagent transcripts DIFFER from main transcripts in parsing

**Yes, the same `SessionMessage` enum is used** — but that is where the similarity ends. The two go through completely different functions.

### Main session line — `project_parser.rs:309-356` → `build_message_event` (`:458-485`) → `message_extractor::project_jsonl_line`

```rust
// message_extractor.rs:50-57
pub fn project_jsonl_line(line: &str) -> Result<MessageProjection, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;          // loose parse (authoritative for msg_type)
    let typed = serde_json::from_str::<SessionMessage>(line); // typed parse (only for FTS)
    Ok(project_parsed_line(&value, &typed))
}
```

- **The raw line is stored verbatim** as `IngestEvent::Message.raw_json` (`project_parser.rs:474`) → `messages.data` column.
- A **typed-parse failure is not fatal**: the row is still written; only `fts_text` is lost, and only *then* is a `RecordSkip` emitted, and only if `fts_text::type_contributes_text(msg_type)` (`message_extractor.rs:89-98`).
- `msg_type`, `uuid`, `timestamp` come from the **loose `Value`**, never from the typed enum (`message_extractor.rs:69-78`).
- Tokens extracted only when `msg_type == "assistant"` (`message_extractor.rs:80-85`, `:113-124`).

### Subagent line — `project_parser.rs:715-761`, `read_one_subagent`

```rust
fn read_one_subagent(
    path: &Path, name: &str, workflow_id: &str,
) -> (SubagentTranscript, Option<DroppedSubagentLines>) {
    let mut messages: Vec<SessionMessage> = Vec::new();
    let mut dropped: Option<DroppedSubagentLines> = None;
    let _ = read_jsonl_streaming(path, 0, |line, idx, _off| {
        match serde_json::from_str::<SessionMessage>(line) {
            Ok(msg) => messages.push(msg),
            Err(e) => {
                let msg_type = serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_default();
                if !fts_text::type_contributes_text(&msg_type) {
                    return;
                }
                match &mut dropped { ... }
            }
        }
    });
```

The concrete differences:

1. **Typed parse is the ONLY parse, and it is load-bearing.** A line that fails `SessionMessage` deserialization is **gone** — there is no `raw_json` fallback. The doc comment is explicit (`project_parser.rs:700-708`):

   > "A dropped line here is worse than on the session path — the transcript is the only thing holding it, so there is no verbatim JSON to fall back on."

2. **Content is RE-SERIALIZED, not stored verbatim.** In `core/writer.rs:1347-1371`:

   ```rust
   for (index, message) in transcript.messages.iter().enumerate() {
       let value = serde_json::to_value(message)?;   // <-- round-trip through the typed enum
       let tokens = subagent_message_tokens(&value, source_id);
       let timestamp = value.get("timestamp").and_then(|c| c.as_str()).map(str::to_owned);
       conn.execute(SQL_INSERT_SUBAGENT_MESSAGE, params![..., serde_json::to_string(&value)?])?;
   }
   ```

   `subagent_messages.data` is the **serde re-encode of the typed struct**, so any JSON key the enum does not model is dropped (mitigated by `#[serde(flatten)] extra` maps on `SystemMessage` — `claude/types/session.rs:400-403` — and `AttachmentPayload` — `session.rs:213-214`). Field ordering and formatting are serde's, not the file's. Timestamp is re-read from that re-serialized value, not from the original line.

3. **Token extraction is separate code**, `core/writer.rs:1151-1171`:

   ```rust
   fn subagent_message_tokens(value: &serde_json::Value, source_id: &str) -> [i64; 4] {
       if source_id != "claude-code" || value.get("type").and_then(|v| v.as_str()) != Some("assistant") {
           return [0, 0, 0, 0];
       }
       ...
   }
   ```
   Same four keys as the main path but reached via the re-serialized value.

4. **No FTS.** `subagent_messages` has no `text_content` column (`core/schema.rs:358-373`); the main `messages` table does.

5. **No `RecordSkip` for malformed JSON**, only for typed failures on content-bearing types. `type_contributes_text` — `claude/fts_text.rs:157-162`:

   ```rust
   pub fn type_contributes_text(msg_type: &str) -> bool {
       matches!(msg_type, "user" | "assistant" | "system" | "summary" | "ai-title")
   }
   ```
   The filter exists because `{}` lines and skills telemetry live in these files and reporting them would withhold the fingerprint forever (`project_parser.rs:704-708`, test at `:1441-1505`).

6. **Read errors are swallowed entirely** — `let _ = read_jsonl_streaming(...)` (`project_parser.rs:722`). The main-session path checks `stream_result` and emits a `RecordSkip` on read failure (`project_parser.rs:362-379`).

7. **`message_count` is `transcript.messages.len()`** (`core/writer.rs:1302`) — the count of *successfully typed* lines, not the count of lines in the file.

8. **Empty transcripts get a dirty-thread marker** rather than being dropped — `core/writer.rs:1372-1387` inserts into `subagent_dirty_threads`.

### Record types that appear

Same enum for both — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/session.rs:23-52`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SessionMessage {
    AgentName(AgentNameMessage),
    Attachment(AttachmentMessage),
    CustomTitle(CustomTitleMessage),
    FileHistorySnapshot(FileHistorySnapshotMessage),
    PrLink(PrLinkMessage),
    Progress(ProgressMessage),
    PermissionMode(PermissionModeMessage),
    #[serde(rename = "saved_hook_context")]
    SavedHookContext(SavedHookContextMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    #[serde(rename = "system")]
    System(SystemMessage),
    #[serde(rename = "summary")]
    Summary(SummaryMessage),
    QueueOperation(QueueOperationMessage),
    LastPrompt(LastPromptMessage),
    AiTitle(AiTitleMessage),
    Mode(ModeMessage),
    BridgeSession(BridgeSessionMessage),
    #[serde(other)]
    Unknown,
}
```

In practice subagent transcripts carry `user` / `assistant` / `system` / `progress`. Two regressions specifically hardened for subagents:

- `AssistantMessage.request_id` was made `#[serde(default)]` because "Absent on some API-error assistant lines; a missing `requestId` must not fail the typed parse (which would null FTS + **drop the line from subagent transcripts**)" — `session.rs:355-359`; test `subagent_keeps_assistant_line_without_usage_or_request_id` at `project_parser.rs:1851-1888`.
- `UserMessage.image_paste_ids` was widened from `Vec<String>` to `Option<Value>` because the wrong annotation "silently emptied its FTS blob and **dropped it from subagent transcripts**" — `session.rs:291-300`.

### Path B: no difference at all

`adapter.rs:729-754` routes both `PARENT_DECODER` and `SUBAGENT_DECODER` into the **same** `decode_transcript_record`; the only distinction is a guard on `agent_id` presence:

```rust
PARENT_DECODER => {
    let object_context = ClaudeTranscriptContext::decode(context.object_context)?;
    if object_context.agent_id.is_some() { return Err(decoder_context_mismatch()); }
    decode_transcript_record(...)
}
SUBAGENT_DECODER => {
    let object_context = ClaudeTranscriptContext::decode(context.object_context)?;
    if object_context.agent_id.is_none() { return Err(decoder_context_mismatch()); }
    decode_transcript_record(...)
}
```

In Path B the raw bytes **are** kept: `raw_json: record.payload.clone()` (`adapter.rs:2032`). Two behavioural branches inside remain keyed on `agent_id`:

- Session-metadata projection (first prompt / titles) is **skipped for subagents** — `adapter.rs:1846-1850`:
  ```rust
  let metadata = if context.agent_id.is_none() {
      session_metadata::project_session_metadata(&value).unwrap_or_default()
  } else {
      session_metadata::SessionMetadataProjection::default()
  };
  ```
- A typed-parse failure emits a scoped diagnostic `claude_typed_projection_loss` rather than dropping the record — `adapter.rs:1969-1992`.

---

## 3. Sidechain handling and parent linkage

### 3.1 `isSidechain` — carried, and used for exactly one thing

It is modelled in two places:

- `BaseMessageFields.is_sidechain` — `claude/types/session.rs:75-76` (`#[serde(default)] pub is_sidechain: bool`), flattened into `user`/`assistant`/`system`/`attachment`/`progress`/`saved_hook_context`.
- `SessionIndexEntry.is_sidechain` — `claude/types/project.rs:55-56`.

**The only place it changes behaviour in this crate** is first-prompt selection — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/session_metadata.rs:57-75`:

```rust
pub fn extract_human_prompt(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) != Some("user")
        || record.get("isMeta").and_then(Value::as_bool) == Some(true)
        || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        || record.get("isVisibleInTranscriptOnly").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let text = first_text_content(record)?;
    if is_synthetic_prompt_text(&text) { return None; }
    Some(crate::core::text::truncate_utf16(&text, 200).to_owned())
}
```

i.e. a sidechain user line can never become a session's `first_prompt` / title.

Everywhere else `is_sidechain` is only **stored and projected**, never used to group or filter:
- `sessions.is_sidechain` column — `core/schema.rs:279`, written at `core/writer.rs:1215`.
- `session_index_entry_assertions.is_sidechain` / `canonical_session_index_entries.is_sidechain` — `core/schema.rs:878`, `:913`; written at `engine/session_index_projection.rs:249` (`i64::from(entry.is_sidechain)`) and carried through the reduction SELECT at `:471-474`.
- Read back for display at `engine/history_query.rs:486`, `:508`, `:587-588`; `engine/detail_query.rs:111`, `:420`, `:494`; `engine/query_pool.rs:260`; `adapter/facts.rs:343`.
- The Codex and Grok readers hard-code `is_sidechain: false` (`codex/reader.rs:464`, `grok/reader.rs:377`).

**There is no "group the sidechain lines of the main transcript into a subagent thread" logic in this crate.** Sidechain grouping is not how subagents are reconstructed here — the filesystem layout is (`subagents/agent-*.jsonl`), plus the linkage rules below.

### 3.2 Linking a subagent to its parent — Path A: content-match on `tool_result`

`core/writer.rs:1090-1149`:

```rust
/// Find the `tool_use_id` of the tool call that spawned a subagent.
///
/// Port of the TS `resolveSubagentSpawnToolId`. Claude records a subagent's
/// result as a `tool_result` block in the parent session whose content
/// mentions the agent id; that block's `tool_use_id` is the spawning call.
/// Without this the whole `subagents.spawn_tool_id` / `link_method` pair was
/// written as `NULL` / `"unlinked"` literals — on a real corpus that silently
/// dropped the linkage for 113 of 638 subagents (RFC 008 Phase 5).
fn resolve_spawn_tool_id(
    conn: &Connection, source_id: &str, session_id: &str, agent_id: &str,
) -> Result<Option<String>, WriterError> {
    if agent_id.is_empty() { return Ok(None); }
    let mut stmt = conn.prepare_cached(
        "SELECT data FROM messages WHERE session_id = ?1 AND source_id = ?2 AND data LIKE '%' || ?3 || '%' ORDER BY msg_index",
    )?;
    let mut rows = stmt.query(params![session_id, source_id, agent_id])?;
    while let Some(row) = rows.next()? {
        let data: String = row.get(0)?;
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(&data) else { continue; };
        let Some(content) = raw.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array())
        else { continue; };
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") { continue; }
            let Some(tool_use_id) = block.get("tool_use_id").and_then(|t| t.as_str()) else { continue; };
            let body = match block.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            if body.contains(agent_id) { return Ok(Some(tool_use_id.to_owned())); }
        }
    }
    Ok(None)
}
```

Consumed at `core/writer.rs:1309-1320`:

```rust
// Resolved here rather than in the parser: the linkage lives in
// the *parent session's* messages, which are already written by
// the time a subagent event arrives ...
let spawn_tool_id = resolve_spawn_tool_id(conn, source_id, session_id, &transcript.agent_id)?;
let link_method = if spawn_tool_id.is_some() { "tool_result" } else { "unlinked" };
```

So Path A's parent link is: **`agent_id` (from filename) → substring search of parent-session `tool_result` bodies → `tool_use_id`**, stored as `subagents.spawn_tool_id` with `link_method ∈ {"tool_result", "unlinked"}` (`core/schema.rs:337-356`). The `LIKE` is only a prefilter, documented as "an optimisation, not the test" (`core/writer.rs:1099-1103`).

Crucially, **`sourceToolUseID` / `sourceToolAssistantUUID` are NOT used for this** — see §3.4.

### 3.3 Linking a subagent to its parent — Path B: layout + tool-call facts

Path B links structurally. The run identity is composed in `adapter.rs:1093-1103`:

```rust
fn run_native_key(&self) -> String {
    match &self.agent_id {
        Some(agent_id) => format!(
            "{}\0{}\0{}",
            self.session_id,
            self.workflow_id.as_deref().unwrap_or(""),
            agent_id
        ),
        None => self.session_id.clone(),
    }
}
```

i.e. `sessionId \0 workflowId \0 agentId`, with an empty middle segment for non-workflow subagents; a root run's key is just the sessionId.

Parent run is derived purely from presence of `agent_id` — `adapter.rs:1890-1899`:

```rust
let parent_run = if context.agent_id.is_some() {
    Some(EntityKey::native(adapter_id, record.source_instance_id, "run",
        context.session_id.as_bytes())?)
} else { None };
```

Role assignment — `adapter.rs:1911-1935`:

```rust
let parent_actor_run = context.agent_id.as_ref()
    .map(|_| output.canonical_root_actor_run_key(context.session_id.as_bytes(), None))
    .transpose()?;
output.push_native(record, run_native_key.as_bytes(),
    Fact::ActorRunRevision(ActorRunRevisionFact {
        actor_run, session: canonical_session,
        role: if context.agent_id.is_some() { ActorRunRole::Child } else { ActorRunRole::Root },
        parent_actor_run,
        native_session_id: Some(context.session_id.clone()),
        native_actor_id: context.agent_id.clone(),
        native_actor_type: None,
    }))?;
```

Delegation fact — `adapter.rs:1938-1959`:

```rust
if let Some(agent_id) = &context.agent_id {
    if !state.delegation_declared || cwd_changed {
        output.push(record, Fact::Delegation(DelegationFact {
            child_run: run.clone(),
            parent_run: delegation_parent,
            session: session.clone(),
            kind: DelegationKind::VendorNativeSubagent,
            relation_strength: RelationStrength::Layout,   // <-- "Layout": from directory structure
            native_child_id: Some(agent_id.clone()),
            native_task_id: None,
            ...
        }))?;
```

Note `native_task_id: None` here — the tool-call side comes separately, from **`Task` / `Agent` tool_use blocks in the parent transcript** — `adapter.rs:2488-2514`:

```rust
fn delegation_spawn_descriptors(content: &[ContentBlock]) -> Vec<DelegationSpawnDescriptor> {
    content.iter().filter_map(|block| {
        let ContentBlock::ToolCall { native_id, name, input } = block else { return None; };
        if !matches!(name.as_str(), "Task" | "Agent") || native_id.trim().is_empty() { return None; }
        Some(DelegationSpawnDescriptor {
            native_task_id: native_id.clone(),
            tool_name: name.clone(),
            label: nonempty_field(input, "description").or_else(|| nonempty_field(input, "name")),
            prompt: nonempty_field(input, "prompt"),
            requested_agent_type: nonempty_field(input, "subagent_type")
                .or_else(|| nonempty_field(input, "agent_type")),
        })
    }).collect()
}
```

emitted as `Fact::DelegationSpawn` keyed `run_native_key ‖ native_task_id` (`adapter.rs:2057-2081`).

The **actual join** between "a spawn happened" and "this transcript is that child" is closed by the meta sidecar's `toolUseId` — `adapter.rs:1763-1776`:

```rust
output.push(record, Fact::DelegationMetadata(DelegationMetadataFact {
    child_run,
    session,
    native_child_id: agent_id.to_string(),
    agent_type,
    description: document.description.as_deref().and_then(nonempty),
    name: native_member_name.clone(),
    spawn_depth: document.spawn_depth,
    worktree_path: document.worktree_path.as_deref().and_then(nonempty),
    native_task_id: document.tool_use_id.as_deref().and_then(nonempty),   // <-- the join key
}))?;
```

### 3.4 Field-by-field verdict

| Field | Where modelled | How actually used |
|---|---|---|
| `agentId` (from filename) | `project_parser.rs:860-868`; `adapter.rs:1071-1077` | **Primary child identity.** Path A: `subagents.agent_id` + substring probe against parent tool_results. Path B: third segment of `run_native_key`, `ActorRunRevisionFact.native_actor_id`, `DelegationFact.native_child_id`. |
| `agentId` (inside a record) | `UserMessage.agent_id` `session.rs:277-278`; `AssistantMessage.agent_id` `session.rs:361-362`; `ProgressMessage.agent_id` `session.rs:431-432`; `AgentProgress.agent_id` `session.rs:478-479` | **Deserialized but never read** in this crate. |
| `agentId` (journal) | `ClaudeWorkflowJournalDocument.agent_id` `sidecars.rs:214` | **Load-bearing** — required non-empty (`sidecars.rs:1100-1107`); forms `child_run_native_key` (`sidecars.rs:1150-1153`) and `WorkflowMemberEventFact.native_agent_id`. |
| `sourceToolUseID` | `UserMessage.source_tool_use_id`, `#[serde(rename = "sourceToolUseID")]` — `session.rs:271-276` | **Declared only.** Zero read sites in the crate (grep-verified). Round-trips through re-serialization. |
| `sourceToolAssistantUUID` | `UserMessage.source_tool_assistant_uuid` — `session.rs:265-270` | **Declared only.** Zero read sites. |
| `parentToolUseID` | `ProgressMessage.parent_tool_use_id` — `session.rs:429-430` | **Declared only.** Zero read sites. |
| `parentUuid` | `BaseMessageFields.parent_uuid` — `session.rs:65-66`; also read loosely at `adapter.rs:1983` | Path B: becomes `MessageFact.parent_native_message_id` (`adapter.rs:2029`) — intra-transcript threading, **not** cross-agent linkage. Path A: stored inside `raw_json` only. |
| `workflowId` | dir name `project_parser.rs:678-680`; `adapter.rs:1078-1080` | Path A: `SubagentTranscript.workflow_id`, part of the `subagents` / `subagent_messages` uniqueness key (`core/schema.rs:355`, `:372`). Path B: middle segment of `run_native_key`. |
| `spawnDepth` | `SubagentMeta.spawn_depth: Option<f64>` `project.rs:103`; `ClaudeSubagentMetadataDocument.spawn_depth: Option<u32>` `sidecars.rs:24`; `DelegationMetadataFact.spawn_depth` `adapter/facts.rs:2063` | Path A: **read but never written** (writer only pulls `agent_type` and `worktree_path` — `core/writer.rs:1296-1308`). Path B: projected to `spawn_depth` column (`core/schema.rs:1219`, `:1284`; `engine/projection.rs:2697`, `:2727`, `:2892`) and surfaced in orchestration/timeline/search queries (`engine/orchestration_query.rs:87`, `:571`, `:1019`; `engine/timeline_query.rs:1398`). |
| `isSidechain` | see §3.1 | Only filters `extract_human_prompt`; otherwise stored/projected. |

---

## 4. `SubagentType` and where nested/workflow subagents differ

### The enum

`/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/project.rs:63-71`:

```rust
/// TS `SubagentType = 'task' | 'prompt_suggestion' | 'compact'`. Kept as a
/// string-backed enum so serde can deserialize the literal values directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentType {
    Task,
    PromptSuggestion,
    Compact,
}
```

### Inference is purely a filename substring test

`project_parser.rs:870-884`:

```rust
/// TS:
/// ```ts
/// if (fileName.includes('prompt_suggestion')) return 'prompt_suggestion';
/// if (fileName.includes('compact')) return 'compact';
/// return 'task';
/// ```
fn infer_agent_type(file_name: &str) -> SubagentType {
    if file_name.contains("prompt_suggestion") {
        SubagentType::PromptSuggestion
    } else if file_name.contains("compact") {
        SubagentType::Compact
    } else {
        SubagentType::Task
    }
}
```

Order matters (`prompt_suggestion` wins), it is `contains` not a prefix/suffix test, and it runs on the whole file name including `agent-` and `.jsonl`.

### The enum is a fallback, not the stored value

`core/writer.rs:1292-1301`:

```rust
// Prefer the sidecar's real agent type (general-purpose, Explore, …)
// over the filename-inferred enum kind. `to_string` on the enum
// produces `"task"` (with quotes) — strip them to store the bare
// string, matching the TS convention.
let agent_type = match transcript.meta.as_ref() {
    Some(meta) => meta.agent_type.clone(),
    None => serde_json::to_string(&transcript.agent_type)?
        .trim_matches('"')
        .to_string(),
};
```

So `subagents.agent_type` is the sidecar's free-form `agentType` (`"Explore"`, `"general-purpose"`, …) whenever `agent-{id}.meta.json` exists and parses; the three-valued enum only surfaces when it does not.

Path B has no equivalent enum at all — it requires a non-empty `agentType` from the sidecar or preserves the record as unknown (`adapter.rs:1736-1744`).

### Where workflow/nested subagents differ

| Aspect | Flat subagent | Workflow-nested subagent |
|---|---|---|
| Location | `<sid>/subagents/agent-*.jsonl` | `<sid>/subagents/workflows/<wf>/agent-*.jsonl` |
| Path A filename filter | any `*.jsonl` (`project_parser.rs:661`) | must start `agent-` (`:688`) |
| Path A depth | flat only | one level only |
| `workflow_id` | `""` (`project_parser.rs:664`, and `types/project.rs:86-89`: *"the `wf_...` run this transcript belongs to, or `""` for a top-level (non-workflow) subagent"*) | directory name (`:691`) |
| `run_native_key` (Path B) | `sid \0 "" \0 agentId` | `sid \0 wf_x \0 agentId` |
| Sidecar `description` | usually present | **absent** — `types/project.rs:97`: *"Absent on nested workflow metas — must be optional or their parse fails."* |
| Extra evidence | none | `journal.jsonl` `started`/`result` events → `Fact::WorkflowMemberEvent` (`sidecars.rs:1069-1219`) |
| Workflow id validation | n/a | Path A none; Path B requires `wf_` + non-empty suffix (`adapter.rs:1566-1574`) |
| Affiliation | none | `ActorAffiliationRevisionFact` with `ActorAffiliationDimension::Workflow` (`sidecars.rs:1202-1217`) |
| Uniqueness | `UNIQUE(source_id, project_slug, session_id, workflow_id, agent_id)` — `core/schema.rs:355` — so the same `agent_id` can legitimately appear once flat and once per workflow |

`WorkflowRun` doc header, `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/workflow.rs:1-6`:

> One agent-orchestration run at `projects/{slug}/{sessionId}/workflows/{runId}.json`. Session-scoped; its nested subagent transcripts group to it via `SubagentTranscript.workflow_id`.

Workflow journal event kinds — `sidecars.rs:1116-1145`: exactly `"started"` (must have **no** `result` key) and `"result"` (must have one); any mismatch or other `type` is a contract loss. Reduced into `member_status ∈ {result_observed, started, orphan_result}` at `engine/workflow_projection.rs:472-481`.

Workflow reconciliation between the run doc's `agentCount` and the observed journal members — `engine/workflow_projection.rs:634-641`:

```rust
let membership_count_status = match (decisive_snapshot, members.observed_count) {
    (_, 0) => "unobserved",
    (None, _) => "snapshot_missing",
    (Some(snapshot), count) if snapshot.agent_count == sqlite_usize(count, "member count")? => "matched",
    (Some(_), _) => "different",
};
```

and the workflow-vs-member identity join check at `:607-621`. Terminal workflow state is deliberately never propagated to child runs (`workflow_projection.rs:5-7`).

---

## 5. How `sessions-index.json` is read and reconciled with actual JSONL

### Path A — read, tolerate, merge, re-serialize

**Read** — `project_parser.rs:489-509`:

```rust
fn read_sessions_index(project_dir: &Path) -> SessionsIndex {
    let path = project_dir.join("sessions-index.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return empty_sessions_index();
    };
    serde_json::from_str::<SessionsIndex>(&raw).unwrap_or_else(|_| empty_sessions_index())
}
```

Missing **and** malformed both degrade to `SessionsIndex { version: 1, original_path: None, entries: vec![] }` (`:503-509`).

**Merge** — `project_parser.rs:511-535`:

```rust
fn merge_with_discovered_entries(
    index_entries: Vec<SessionIndexEntry>,
    project_dir: &Path,
    original_path: Option<&str>,
) -> Result<Vec<SessionIndexEntry>, ParseError> {
    let mut indexed: HashSet<String> = index_entries.iter().map(|e| e.session_id.clone()).collect();
    let mut merged = index_entries;

    // Sorted, because the source is a directory listing and neither engine
    // gets a guaranteed order from one. NTFS returns entries sorted while ext4
    // and APFS do not, so an unsorted merge agreed on Windows and disagreed on
    // Linux and macOS ... (RFC 008 Phase 5).
    let mut discovered = discover_session_entries(project_dir, original_path)?;
    discovered.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    for entry in discovered {
        if indexed.insert(entry.session_id.clone()) {
            merged.push(entry);
        }
    }
    Ok(merged)
}
```

Index entries win; discovered-only entries are appended, sorted, after them. Nothing is ever removed for being index-only-with-no-file.

**Discover** — `project_parser.rs:545-636`. Directory-enumeration failure is **project-fatal**, not a skipped record:

```rust
// If it fails we cannot know, and a merged index built from the
// sessions-index alone would look complete while silently omitting every
// session on disk — so this is fatal to the project rather than a skipped
// record (RFC 008 Phase 2).
//
// A missing directory is not a failure: the slug came from a directory
// listing, so this means it was deleted mid-run ...
let read_dir = match std::fs::read_dir(project_dir) {
    Ok(rd) => rd,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(e) => return Err(ParseError::Fatal { path: ..., message: e.to_string() }),
};
```

Stub entries are filled from file mtime with an explicitly Node-compatible ms computation (`project_parser.rs:596-606`):

```rust
// Node computes `stats.mtimeMs` as `sec * 1000 + nsec / 1e6`. Computing
// `as_secs_f64() * 1000.0` instead rounds differently in the last
// millisecond ... a visible 1 ms disagreement on session timestamps (RFC 008 Phase 5).
.map(|d| d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1_000_000.0)
```

plus `first_prompt` from `peek_first_user_prompt` (`:616`, impl `:1199-1215`, truncated to 200 UTF-16 units) defaulting to `"No prompt"`, and `summary: Some(String::new())` — deliberately `Some("")` not `None`, because "The real corpus has both — 6 NULL and 117 empty" (`types/project.rs:33-42`).

**Persist the MERGED index** — `project_parser.rs:178-189`:

```rust
// 3. Emit the Project event storing the serialized MERGED index
//    (parity with TS `onProject`, which stores
//    `JSON.stringify(parseSessionsIndex(...))`). Storing the raw file
//    text here would drop discovered-but-unindexed sessions from
//    `projects.sessions_index`.
let sessions_index_json = serde_json::to_string(&merged_index).unwrap_or_else(|_| "{}".to_owned());
```

Test `no_sessions_index_stores_merged_discovered_entries` at `project_parser.rs:1735-1777` pins this.

**Per-session file resolution** — `project_parser.rs:290-299`:

```rust
// Canonical path, with fallback to entry.full_path if the canonical
// file doesn't exist (handles stale indices pointing at relocated
// JSONL files). Port of the TS `filePath` ternary.
let canonical_path = project_dir.join(format!("{session_id}.jsonl"));
let file_path: PathBuf = if canonical_path.exists() {
    canonical_path.clone()
} else if !entry.full_path.is_empty() && Path::new(&entry.full_path).exists() {
    PathBuf::from(&entry.full_path)
} else {
    canonical_path.clone()
};
```

An index entry with no file on disk still emits `IngestEvent::Session` + `SessionComplete` with `message_count = 0` — sessions are never dropped for lack of a transcript.

`original_path`: `sessions_index.original_path` if present, else `slug_to_path(slug)` (`project_parser.rs:161-164`); the same fallback is applied per-entry inside discovery (`:575-582`).

### Path B — index is Supplemental, never authoritative

`adapter.rs:557-573` declares `SESSION_INDEX_STREAM` as `StreamAuthority::Supplemental`, `EntityScope::Project`, `ConsistencyPolicy::SnapshotReplace`, cap 1 MiB (`adapter.rs:165`).

Decoding is **strict** — `sidecars.rs:1221-1343`. Any of these preserves the whole document as a contract loss rather than partially applying it: not JSON; not the expected shape; `version != 1`; `> 4096` entries (`SESSION_INDEX_ENTRY_LIMIT`, `adapter.rs:173`); blank `originalPath`; a non-UUID `sessionId`; duplicate `sessionId`; an entry with empty `fullPath` / `firstPrompt` / `created` / `modified` / `projectPath` (`sidecars.rs:1290-1304`).

The projection is where reconciliation happens — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/engine/session_index_projection.rs`. Module doc, `:1-7`:

> A native index is useful metadata, but it is not transcript evidence. Index entries therefore remain queryable when their JSONL is absent and join to canonical transcript sessions only when an independently committed `SessionFact` exists.

The join is a left-lookup against `canonical_sessions` — `session_index_projection.rs:440-457`:

```rust
let transcript_project = transaction
    .query_row(
        "SELECT project_key FROM canonical_sessions WHERE session_key = ?1",
        [session_key],
        |row| row.get::<_, Vec<u8>>(0),
    )
    .optional()
    .map_err(|error| sqlite_error("join session index entry to transcript", error))?;
let (transcript_status, join_conflict) = match transcript_project {
    Some(project_key) if project_key != *decisive_project_key => ("different_project", true),
    Some(_) => ("present", false),
    None => ("missing", false),
};
let resolution_status = if competing_entry_count > 0 || identity_conflict || join_conflict {
    "conflicting"
} else {
    "resolved"
};
```

So each `canonical_session_index_entries` row carries `transcript_status ∈ {present, missing, different_project}` and `resolution_status ∈ {resolved, conflicting}` plus `assertion_count`, `competing_entry_count`, `identity_conflict`, `join_conflict` (`:458-514`). Project-level snapshots compete by digest, `competing_snapshot_count = distinct digests − 1`, status `conflicting`/`resolved` (`:293-304`). Changes are published as `history.session-index.changed`, `diagnostic.history.session-index-conflict`, `history.session-index-entry.changed`, `diagnostic.history.session-index-entry-conflict` (`:531`, `:551`, `:566`, `:589`).

Snapshot ownership matters — `session_index_projection.rs:82-92`:

```rust
// A transcript commit may still change a session-index join, but it does
// not own the index document. Only replace assertions when this object
// supplied (or previously supplied) the replace-document snapshot.
if owns_snapshot { ... DELETE FROM session_index_snapshot_assertions WHERE source_object_id = ?1 ... }
```

### Path B — catalog discovery reconciliation

`/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/catalog_discovery.rs` is the third read of the index, at catalog level. Its header states the filesystem-is-the-truth stance (`:6-24`), including:

> Nested `subagents/agent-*.jsonl` transcripts belong to their parent session and are deliberately not catalog members.

Scan is depth-pinned to 2 (`:70-76`) with a selector that includes session directories but never recurses into them — `catalog_discovery.rs:250-270`:

```rust
/// Depth-2 selection: project directories and, inside each, the transcripts,
/// session directories, and session index. Session directories are recorded
/// but never descended into, so a deep `subagents/` or `workflows/` tree
/// costs nothing.
fn catalog_selection(path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    ...
    match (kind, components.len()) {
        (DirectoryEntryKind::Directory, 1) => DirectorySelection::Recurse,
        (DirectoryEntryKind::Directory, 2) if is_uuid(&components[1]) => DirectorySelection::Include,
        (DirectoryEntryKind::File, 2) => {
            let name = &components[1];
            if name == SESSION_INDEX_NAME || name.ends_with(".jsonl") {
                DirectorySelection::Include
            } else { DirectorySelection::Ignore }
        }
        _ => DirectorySelection::Ignore,
    }
}
```

Three evidence classes with a precedence order:
- **Session directory** `<slug>/<uuid>/` → `ProjectAssociationBasis::SessionDirectory`, `AssociationQuality::Exact`, `transcript_present: false` (`:122-132`, `:222-244`).
- **Transcript file** `<slug>/<uuid>.jsonl` → same basis, but with size/mtime and `transcript_present: true` (`:138-166`).
- **Index entry** → `ProjectAssociationBasis::NativeProjectIndex`, `AssociationQuality::NativeClaimed`, `transcript_present: false`.

Merge rules — `merge_index_entry` (`:275-317`) *refines* an existing session (title/created/modified/messageCount fill only if absent) and *introduces* index-only sessions, but never overwrites the directory association; a project mismatch produces a `DiscoveredAssociationConflict`. `insert_session` (`:326-364`) merges agreeing evidence (the transcript upgrades the directory row) and records the loser as a conflict when projects disagree, keeping the higher `association_quality`.

Index parse here is lenient (a bad index only sets `degraded_reason`) — `:366-403`, with `title` = `summary` else `firstPrompt` (`:395`).

Fallback for the increasingly common no-index case — `catalog_discovery.rs:186-200`:

```rust
// Current Claude installations commonly omit sessions-index.json. The
// slug cannot be reversed safely because '-' may be either a path
// separator or part of a directory name. Use the catalog contract's one
// bounded record-head read instead ... Index evidence wins when it exists.
for (slug, candidate) in transcript_heads {
    ...
    if project.display_path.is_none() {
        project.display_path = read_transcript_cwd(root, &candidate.relative_path, limits.max_head_bytes);
    }
}
```

`read_transcript_cwd` (`:212-219`) reads a bounded prefix of the **newest** transcript (chosen at `:145-155` by `(modified_ns, relative_path)`) and takes the first record's `cwd`.

---

## 6. Project-slug ↔ cwd path encoding/decoding

There is **no encoder** in this crate (encoding is Claude Code's job); there is one decoder, and one deliberate refusal to decode.

### The decoder: `slug_to_path` — `project_parser.rs:1153-1192`

Two-stage. First `slug_shape` classifies the platform — `project_parser.rs:1094-1131`. Its doc table (`:1076-1093`):

```
/// | cwd                      | slug                     | shape         |
/// |--------------------------|--------------------------|---------------|
/// | `/Users/me/app`          | `-Users-me-app`          | `/`, `/`      |
/// | `D:\Projects\app`        | `D--Projects-app`        | `D:\`, `\`    |
/// | `D:\Projects\app` (old)  | `D:-Projects-app`        | `D:\`, `\`    |
```

> A POSIX cwd is always absolute, so it always yields a leading `-`. That makes "no leading dash, but starts with `<letter>--` or `<letter>:-`" an unambiguous Windows drive marker. ... Detection is platform-independent on purpose — a synced `~/.claude` must decode to the same path text on any host.

```rust
fn slug_shape(slug: &str) -> SlugShape<'_> {
    let bytes = slug.as_bytes();
    // `X--…` or `X:-…` — a Windows drive letter. Length 3 is the bare
    // drive root (`D--` → `D:\`), so `>=` not `>`.
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && (bytes[1] == b':' || bytes[1] == b'-')
        && bytes[2] == b'-'
    {
        let drive = (bytes[0] as char).to_ascii_uppercase();
        return SlugShape { prefix: format!("{drive}:\\"), sep: '\\', rest: &slug[3..] };
    }
    if let Some(rest) = slug.strip_prefix('-') {
        return SlugShape { prefix: "/".to_string(), sep: '/', rest };
    }
    SlugShape { prefix: String::new(), sep: '/', rest: slug }
}
```

Order matters: the drive test runs first, but a leading `-` cannot reach it (`-` is not `is_ascii_alphabetic`), so POSIX always wins for absolute slugs — pinned by `slug_to_path_prefers_posix_when_leading_dash_present` (`:1961-1965`: `"-d--Projects-app"` → `"/d//Projects/app"`).

Second stage is a **filesystem probe**, longest-match-first, because the encoding is lossy — `project_parser.rs:1133-1152`:

> Claude Code encodes project paths into slugs by replacing every path separator with `-`. Reversing that naively (every `-` → separator) corrupts legitimate hyphens ... e.g. `-Users-me-Projects-vibe-ctl` can decode to either `/Users/me/Projects/vibe/ctl` or `/Users/me/Projects/vibe-ctl` depending on what actually exists on disk.
>
> We resolve the ambiguity the same way the TS parser does: probe the filesystem from left to right. At each position, try the longest remaining suffix first ... For slugs whose underlying project dir no longer exists ... we degrade gracefully to the naive one-segment-per-dash mapping — the display path may be wrong but ingest still works.

```rust
let parts: Vec<&str> = rest.split('-').collect();
let mut resolved = String::new();
let mut i = 0;
while i < parts.len() {
    let mut matched = false;
    for end in (i + 1..=parts.len()).rev() {          // longest first
        let candidate_segment = parts[i..end].join("-");
        let probe_path = if resolved.is_empty() {
            format!("{prefix}{candidate_segment}")
        } else {
            format!("{prefix}{resolved}{sep}{candidate_segment}")
        };
        if std::fs::metadata(&probe_path).is_ok() {
            if !resolved.is_empty() { resolved.push(sep); }
            resolved.push_str(&candidate_segment);
            i = end; matched = true; break;
        }
    }
    if !matched {
        if !resolved.is_empty() { resolved.push(sep); }
        resolved.push_str(parts[i]);
        i += 1;
    }
}
format!("{prefix}{resolved}")
```

**Note the probe is greedy per-position with no backtracking** — once a segment is accepted it is never revisited, so a wrong-but-existing longest match can strand the remainder into the naive fallback.

Callers: `project_parser.rs:164` (project `original_path` when the index has none) and `:578` (per-entry `project_path` fallback, derived from `project_dir.file_name()`).

Tests: `:1931-1984` — POSIX (`"-"` → `"/"`, `""` → `""`), Windows both spellings, uppercase normalization of the drive letter (`"d--Projects-app"` → `"D:\\Projects\\app"`), bare roots, and a live-filesystem probe test proving `p100-app` survives as one segment (`:1971-1984`).

### The refusal

`catalog_discovery.rs:186-191` deliberately does **not** call any decoder:

> The slug cannot be reversed safely because '-' may be either a path separator or part of a directory name. Use the catalog contract's one bounded record-head read instead: it stays confined to the authorized projects root and gives the same cwd evidence the full decoder will later commit. Index evidence wins when it exists.

Precedence for `DiscoveredProject.display_path`: `sessions-index.json` `entries[].projectPath` (`catalog_discovery.rs:168-184`), else the newest transcript's `cwd` (`:192-200`), else `None`. The slug is never reversed.

The adapter/fact path likewise never decodes: `native_project_key` **is** the slug verbatim (`adapter.rs:1878`, `catalog_discovery.rs:113-117`, and `catalog_discovery.rs:20-22`: *"`<project-slug>` is byte-identical to the `native_project_key` the Claude decoder derives from the same path"*). Path B gets cwd from the record itself — `adapter.rs:1851` `let cwd = nonempty_field(&value, "cwd");` → `SessionFact.cwd` (`:1879`), with change detection so a moved workspace re-declares (`:1853-1854`, `:1864`).

Project enumeration is just a directory listing — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/orchestrate/ingest.rs:1043-1064`:

```rust
/// List immediate subdirectories of `<agent_dir>/projects/`. Each dir
/// name is a project slug. Non-directory entries (e.g. `.DS_Store`) are
/// skipped silently.
fn scan_project_slugs(agent_dir: &Path) -> std::result::Result<Vec<String>, std::io::Error> { ... slugs.sort(); ... }
```

Unrelated but easy to confuse: `platform_path_key` (`/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/source/file.rs:465-473`) is a length-prefixed byte encoding of a path used as a *source-instance identity key* — not a slug codec.

---

## 7. Divergences and sharp edges worth flagging

1. **`^agent-(a.+)\.jsonl$` vs `^agent-(.+)\.jsonl$` vs `strip_prefix("agent-")`.** `project_parser.rs:50` requires the agent id to begin with `a`; `fingerprint.rs:94` and `adapter.rs:1072` do not. An id not starting with `a` gets `agent_id = "agent-<id>"` in Path A and `"<id>"` in Path B, and the `subagents.agent_id` value then fails to substring-match the `tool_result` body in `resolve_spawn_tool_id` (`core/writer.rs:1143`), silently downgrading `link_method` to `"unlinked"`.
2. **Path A's nested walk is one level deep** (`project_parser.rs:682-694`) while the declared glob and Path B allow `subagents/workflows/*/**/agent-*.jsonl` (`adapter.rs:354`, fixture `adapter.rs:4058`). Path A misses `.../wf_main/agents/agent-child.jsonl`.
3. **Path A's flat walk accepts any `*.jsonl`** under `subagents/` (`project_parser.rs:661`), so a stray non-`agent-` file becomes a `SubagentTranscript` whose `agent_id` is the bare stem.
4. **Subagent messages are re-serialized, not stored verbatim** (`core/writer.rs:1348`, `:1368`) — unmodelled JSON keys are lost except where a `#[serde(flatten)] extra` map catches them.
5. **`sourceToolUseID`, `sourceToolAssistantUUID`, `parentToolUseID` are dead weight** in this crate — declared for shape fidelity, read nowhere. The parent link is filename-`agentId` + `tool_result` substring (Path A) or directory layout + `meta.json` `toolUseId` (Path B). If someone expects these fields to be the join key, they are not.
6. **`spawnDepth` is parsed but discarded on Path A** — `SubagentMeta.spawn_depth` (`types/project.rs:103`) is never read by `core/writer.rs`; only Path B's `DelegationMetadataFact` persists it.
7. **`workflow_id` is unvalidated on Path A** (any directory name under `workflows/`), strictly `wf_`-prefixed on Path B (`adapter.rs:1566-1574`), and Path A takes it from the JSON body's `runId` for run docs while Path B rejects a `runId`/filename mismatch (`sidecars.rs:994-1001`).
8. **`isSidechain` does no grouping anywhere in this crate.** Its only behavioural effect is disqualifying a line from becoming `first_prompt` (`session_metadata.rs:60`).
# Claude Code subagent-transcript discovery & parsing in `spaghetti-napi`

All paths are relative to `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/`. Absolute paths given at first mention.

---

## 0. Orientation — there are TWO parallel ingest paths in this crate

Both are live and answer the same questions differently:

| | **Path A — legacy stream parser** | **Path B — adapter / ADS** |
|---|---|---|
| Entry | `ProjectParser::parse_project` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/project_parser.rs:116` | `ClaudeCodeAdapter::streams` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter.rs:328` |
| Discovery | `std::fs::read_dir` walks, hand-rolled per artifact type | Declarative `ObjectSelector { root_name, include: [glob] }` |
| Output | `IngestEvent` over `crossbeam_channel` → `core/writer.rs` → tables `subagents`, `subagent_messages`, `workflows` | `Fact::*` → `engine/*_projection.rs` → `canonical_*` tables |
| Warm start | `claude/fingerprint.rs` (`source_files` categories) | catalog + cursors (`claude/catalog_discovery.rs`) |
| Ported from | `packages/sdk/src/parser/project-parser.ts` (comments say so verbatim) | RFC 012A/012C native fact model |

Path A is the PRIMARY FILE. Path B's globs are the authoritative "glob strings" answer.

---

## 1. Path patterns, globs, and ID extraction

### 1.1 The declarative globs (Path B — `claude/adapter.rs`)

Literal strings. Root `"projects"` = `<claude_home>/projects`.

```rust
// adapter.rs:347-357  — SUBAGENT_STREAM
selector: ObjectSelector {
    root_name: "projects".to_string(),
    include: vec![
        "*/*/subagents/**/agent-*.jsonl".to_string(),
        "*/*/subagents/workflows/*/**/agent-*.jsonl".to_string(),
    ],
    exclude: Vec::new(),
},
```

```rust
// adapter.rs:367-376 — SUBAGENT_META_STREAM
include: vec!["*/*/subagents/**/agent-*.meta.json".to_string()],
```

```rust
// adapter.rs:539-546 — WORKFLOW_JOURNAL_STREAM
include: vec!["*/*/subagents/workflows/*/journal.jsonl".to_string()],
```

Full glob table (`adapter.rs`):

| Stream const | line | root | include glob(s) |
|---|---|---|---|
| `PARENT_STREAM` (`session-transcripts`) | 335 | projects | `*/*.jsonl` |
| `SUBAGENT_STREAM` | 353-354 | projects | `*/*/subagents/**/agent-*.jsonl`, `*/*/subagents/workflows/*/**/agent-*.jsonl` |
| `SUBAGENT_META_STREAM` | 374 | projects | `*/*/subagents/**/agent-*.meta.json` |
| `WORKFLOW_RUN_STREAM` | 527 | projects | `*/*/workflows/wf_*.json` |
| `WORKFLOW_JOURNAL_STREAM` | 544 | projects | `*/*/subagents/workflows/*/journal.jsonl` |
| `SESSION_INDEX_STREAM` | 563 | projects | `*/sessions-index.json` |
| `PROJECT_MEMORY_STREAM` | 582 | projects | `*/memory/*.md` |
| `PERSISTED_TOOL_RESULT_STREAM` | 601 | projects | `*/*/tool-results/*.txt` |
| `TODO_STREAM` | 451 | home | `todos/*-agent-*.json` |
| `TASK_ITEM_STREAM` | 470 | home | `tasks/*/*.json` |
| `PLAN_STREAM` | 489 | home | `plans/*.md` |
| `ARTIFACT_CONTENT_STREAM` | 508 | home | `file-history/*/*@v*` |
| `TEAM_CONFIG_STREAM` | 393 | teams | `*/config.json` |
| `TEAM_INBOX_STREAM` | 412 | teams | `*/inboxes/*.json` |
| `ACTIVE_SESSION_STREAM` | 432 | sessions | `*.json` |
| `INTERPRETATION_SETTINGS_STREAM` | 620-623 | home | `settings.json`, `settings.local.json` |

Roots declared at `adapter.rs:296-313`: `home` = `~/.claude`, `projects` = `home/projects`, `teams` = `home/teams`, `sessions` = `home/sessions`.

### 1.2 The imperative walks (Path A — `project_parser.rs`)

`read_subagents` does **two** walks and they are not symmetric:

```rust
// project_parser.rs:647-698
fn read_subagents(
    project_dir: &Path,
    session_id: &str,
) -> Vec<(SubagentTranscript, Option<DroppedSubagentLines>)> {
    let dir = project_dir.join(session_id).join("subagents");
    let mut out = Vec::new();

    // Top-level subagent transcripts (not associated with a workflow).
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            ...
            if !name.ends_with(".jsonl") {        // <-- ANY *.jsonl, no agent- prefix required
                continue;
            }
            out.push(read_one_subagent(&entry.path(), name, ""));
        }
    }

    // Nested workflow subagent transcripts:
    //   subagents/workflows/{wf_id}/agent-*.jsonl  (journal.jsonl is skipped
    //   by the `agent-` prefix). ...
    if let Ok(wf_dirs) = std::fs::read_dir(dir.join("workflows")) {
        for wf_entry in wf_dirs.flatten() {
            if !wf_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
            let wf_name = wf_entry.file_name();
            let Some(workflow_id) = wf_name.to_str() else { continue; };
            if let Ok(agent_dir) = std::fs::read_dir(wf_entry.path()) {
                for entry in agent_dir.flatten() {
                    ...
                    if !name.starts_with("agent-") || !name.ends_with(".jsonl") { continue; }
                    out.push(read_one_subagent(&entry.path(), name, workflow_id));
                }
            }
        }
    }
    out
}
```

Asymmetries:
- **Top-level** (`project_parser.rs:661`): accepts **any** `*.jsonl` in `subagents/`, not just `agent-*`.
- **Nested** (`project_parser.rs:688`): requires the `agent-` prefix — that is what excludes `journal.jsonl`.
- **Nested walk is only one level deep** — `subagents/workflows/<wf>/agent-*.jsonl` only. The adapter glob `*/*/subagents/workflows/*/**/agent-*.jsonl` allows deeper (e.g. `.../wf_main/agents/agent-child.jsonl`, exercised at `adapter.rs:4058` and `adapter.rs:5852`). **Path A misses those; Path B catches them.** Same for `**` in the flat glob.
- `workflow_id` is literally the directory name — no validation in Path A (Path B requires `wf_`, see §4).

### 1.3 `agent-{id}.meta.json` sidecar

Path A reads it by suffix substitution, not glob:

```rust
// project_parser.rs:746-749
// Sibling `agent-{id}.meta.json` carries the real agent type + description.
let meta = std::fs::read_to_string(path.with_extension("meta.json"))
    .ok()
    .and_then(|raw| serde_json::from_str::<SubagentMeta>(&raw).ok());
```

`Path::with_extension` replaces only the final extension, so `agent-a1.jsonl` → `agent-a1.meta.json`. A malformed or missing sidecar is silently `None` (no `RecordSkip`).

`SubagentMeta` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/project.rs:93-106`:

```rust
pub struct SubagentMeta {
    pub agent_type: String,
    // Absent on nested workflow metas — must be optional or their parse fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub name: Option<String>,
    pub spawn_depth: Option<f64>,
    pub worktree_path: Option<String>,
}
```

Path B's richer version adds `teamName` and `toolUseId` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter/sidecars.rs:13-29`:

```rust
pub(super) struct ClaudeSubagentMetadataDocument {
    pub(super) agent_type: String,
    pub(super) description: Option<String>,
    pub(super) name: Option<String>,
    pub(super) team_name: Option<String>,
    pub(super) spawn_depth: Option<u32>,
    pub(super) worktree_path: Option<String>,
    pub(super) tool_use_id: Option<String>,
}
```

Real fixture at `adapter.rs:7045`:
```
{"agentType":"Explore","description":"map the parser","name":"survey","spawnDepth":2,"worktreePath":"/repo/.worktrees/a1","toolUseId":"tool-7"}
```

### 1.4 `agentId` extraction from the filename

**Path A** — `project_parser.rs:49-50` and `860-868`:

```rust
/// Matches `agent-<id>.jsonl` where `<id>` starts with `a`. Port of
/// `extractAgentId`'s `^agent-(a.+)\.jsonl$` pattern.
static SUBAGENT_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^agent-(a.+)\.jsonl$").expect("SUBAGENT_FILE regex compiles"));
```

```rust
/// TS: `const match = fileName.match(/^agent-(a.+)\.jsonl$/);`
fn extract_agent_id(file_name: &str) -> String {
    if let Some(caps) = SUBAGENT_FILE.captures(file_name) {
        caps.get(1)
            .map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| file_name.trim_end_matches(".jsonl").to_owned())
    } else {
        file_name.trim_end_matches(".jsonl").to_owned()
    }
}
```

The `a` inside `(a.+)` is **part of the capture group**, so `agent-abc123.jsonl` → `"abc123"` (asserted at `project_parser.rs:1435`). But the id **must start with `a`** or the regex fails and the fallback yields the **whole stem including the `agent-` prefix**: `agent-xyz.jsonl` → `"agent-xyz"`.

**Real cross-path divergence.** Compare:
- `claude/fingerprint.rs:93-94`: `^agent-(.+)\.jsonl$` — no `a` constraint.
- `adapter.rs:1071-1077`: pure strip, no `a` constraint:

```rust
let Some(agent_id) = file_name
    .strip_prefix("agent-")
    .and_then(|name| name.strip_suffix(suffix))
    .filter(|value| !value.is_empty())
else {
    return Err(path_error(relative_path, "invalid subagent object name"));
};
```

For a hypothetical `agent-b7.jsonl`, Path A stores `agent_id = "agent-b7"` and Path B stores `"b7"`.

### 1.5 `sessionId` / `workflowId` extraction from directories

**Path A**: `session_id` is never parsed out of a subagent path — passed down from the enclosing session loop (`project_parser.rs:382`, `read_subagents(project_dir, &session_id)`). `workflow_id` is the raw directory name (`project_parser.rs:678-680`).

**Path B** — `adapter.rs:1062-1087`:

```rust
fn subagent_object(relative_path: &Path, suffix: &str) -> Result<Self, AdapterError> {
    let components = utf8_components(relative_path)?;
    if components.len() < 4 || components.get(2).map(String::as_str) != Some("subagents") {
        return Err(path_error(relative_path, "subagent object path does not match Claude layout"));
    }
    let file_name = components.last().expect("minimum component count checked");
    let Some(agent_id) = file_name
        .strip_prefix("agent-")
        .and_then(|name| name.strip_suffix(suffix))
        .filter(|value| !value.is_empty())
    else { return Err(path_error(relative_path, "invalid subagent object name")); };
    let workflow_id = components.windows(2).find_map(|pair| {
        (pair[0] == "workflows" && !pair[1].is_empty()).then(|| pair[1].clone())
    });
    Ok(Self {
        project_slug: components[0].clone(),
        session_id: components[1].clone(),
        agent_id: Some(agent_id.to_string()),
        workflow_id,
    })
}
```

`components[0] = project_slug`, `components[1] = session_id`, `components[2]` must literally be `"subagents"`, `workflow_id` = the component immediately after any component named `workflows` (sliding window, so it works at any depth).

Suffix dispatch: `.jsonl` for transcripts (`adapter.rs:1054-1056`), `.meta.json` for sidecars (`adapter.rs:1058-1060`).

Workflow journal path is a strict fixed shape — `adapter.rs:1534-1552`:

```rust
fn journal_from_path(relative_path: &Path) -> Result<Self, AdapterError> {
    let components = utf8_components(relative_path)?;
    if components.len() != 6
        || components[2] != "subagents"
        || components[3] != "workflows"
        || components[5] != "journal.jsonl"
    {
        return Err(path_error(relative_path,
            "workflow journal path must be <project>/<session>/subagents/workflows/<workflow>/journal.jsonl"));
    }
    Self::validate(relative_path, &components[0], &components[1], &components[4])
}
```

Workflow run doc path — `adapter.rs:1515-1532`: exactly 4 components, `components[2] == "workflows"`, `runId` = `components[3]` minus `.json`.

`validate` (`adapter.rs:1554-1580`): non-empty slug, `is_uuid(session_id)`, `native_workflow_id.strip_prefix("wf_")` with non-empty suffix.

### 1.6 `workflows/{runId}.json` in Path A

`project_parser.rs:764-821`:

```rust
if !name.starts_with("wf_") || !name.ends_with(".json") { continue; }   // :776
...
let workflow_id = data
    .get("runId")
    .and_then(|v| v.as_str())
    .map(str::to_owned)
    .unwrap_or_else(|| name.trim_end_matches(".json").to_owned());      // :786-790
```

Workflow id comes from the **JSON body's `runId`**, filename stem as fallback. (Path B instead *rejects* the document when `runId != filename` — `sidecars.rs:994-1001`.)

`subagent_count` is recomputed by re-counting the directory (`project_parser.rs:802`, `count_workflow_subagents` at `:823-841`, requires `agent-` prefix + `.jsonl`). Journal read via `read_workflow_journal` (`:843-857`) at `<session>/subagents/workflows/<wf>/journal.jsonl` into `Vec<Value>`, silently dropping unparseable lines. Runs sorted by `workflow_id` (`:819`).

### 1.7 Other artifact patterns in Path A

| Artifact | Location | Pattern | Code |
|---|---|---|---|
| session JSONL | `projects/<slug>/` | `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$` | `project_parser.rs:42-45` |
| tool results | `projects/<slug>/<sid>/tool-results/` | `*.txt`, `tool_use_id` = stem | `:888-913` |
| file history | `<root>/file-history/<sid>/` | `^([0-9a-f]+)@v(\d+)$` | `:54-56`, `:917-968` |
| todos | `<root>/todos/` | prefix `"{session_id}-agent-"` + `.json`, then `^(.+?)-agent-(.+)\.json$` | `:60-61`, `:972-1013` |
| tasks | `<root>/tasks/<sid>/.lock`, `.highwatermark` | existence + `parseInt`-lenient | `:1017-1040` |
| plans | `<root>/plans/*.md` | title = first `(?m)^#\s+(.+)$` | `:1222-1275` |
| memory | `projects/<slug>/memory/MEMORY.md` | fixed | `:640-643` |

Warm-start categories — `claude/fingerprint.rs:30-40` documents the taxonomy; subagent constants at `fingerprint.rs:62-82`, with `CATEGORY_SUBAGENT_META` and `CATEGORY_WORKFLOW_JOURNAL` added because "Discovery previously saw only the flat `agent-*.jsonl`, so a changed agent type, a changed journal, or a whole workflow's transcripts were invisible to warm start — RFC 008 Phase 1.4" (`fingerprint.rs:608-616`). The fingerprint walk mirrors the parser at `fingerprint.rs:617-712`.

---

## 2. How subagent transcripts DIFFER from main transcripts in parsing

**Yes, the same `SessionMessage` enum is used** — but that is where the similarity ends.

### Main session line — `project_parser.rs:309-356` → `build_message_event` (`:458-485`) → `message_extractor::project_jsonl_line`

```rust
// message_extractor.rs:50-57
pub fn project_jsonl_line(line: &str) -> Result<MessageProjection, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;          // loose parse (authoritative for msg_type)
    let typed = serde_json::from_str::<SessionMessage>(line); // typed parse (only for FTS)
    Ok(project_parsed_line(&value, &typed))
}
```

- **Raw line stored verbatim** as `IngestEvent::Message.raw_json` (`project_parser.rs:474`) → `messages.data`.
- A **typed-parse failure is not fatal**: the row is still written; only `fts_text` is lost, and only then is a `RecordSkip` emitted, and only if `fts_text::type_contributes_text(msg_type)` (`message_extractor.rs:89-98`).
- `msg_type`, `uuid`, `timestamp` come from the **loose `Value`**, never the typed enum (`message_extractor.rs:69-78`).
- Tokens extracted only when `msg_type == "assistant"` (`message_extractor.rs:80-85`, `:113-124`).

### Subagent line — `project_parser.rs:715-761`, `read_one_subagent`

```rust
fn read_one_subagent(
    path: &Path, name: &str, workflow_id: &str,
) -> (SubagentTranscript, Option<DroppedSubagentLines>) {
    let mut messages: Vec<SessionMessage> = Vec::new();
    let mut dropped: Option<DroppedSubagentLines> = None;
    let _ = read_jsonl_streaming(path, 0, |line, idx, _off| {
        match serde_json::from_str::<SessionMessage>(line) {
            Ok(msg) => messages.push(msg),
            Err(e) => {
                let msg_type = serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_default();
                if !fts_text::type_contributes_text(&msg_type) {
                    return;
                }
                match &mut dropped { ... }
            }
        }
    });
```

Concrete differences:

1. **Typed parse is the ONLY parse, and it is load-bearing.** A line failing `SessionMessage` deserialization is **gone** — no `raw_json` fallback. Doc comment is explicit (`project_parser.rs:700-708`):

   > "A dropped line here is worse than on the session path — the transcript is the only thing holding it, so there is no verbatim JSON to fall back on."

2. **Content is RE-SERIALIZED, not stored verbatim.** `core/writer.rs:1347-1371`:

   ```rust
   for (index, message) in transcript.messages.iter().enumerate() {
       let value = serde_json::to_value(message)?;   // <-- round-trip through the typed enum
       let tokens = subagent_message_tokens(&value, source_id);
       let timestamp = value.get("timestamp").and_then(|c| c.as_str()).map(str::to_owned);
       conn.execute(SQL_INSERT_SUBAGENT_MESSAGE, params![..., serde_json::to_string(&value)?])?;
   }
   ```

   `subagent_messages.data` is the **serde re-encode of the typed struct**, so any JSON key the enum does not model is dropped (mitigated by `#[serde(flatten)] extra` on `SystemMessage` — `claude/types/session.rs:400-403` — and `AttachmentPayload` — `session.rs:213-214`). Field ordering is serde's, not the file's. Timestamp is re-read from that re-serialized value, not from the original line.

3. **Token extraction is separate code**, `core/writer.rs:1151-1171`:

   ```rust
   fn subagent_message_tokens(value: &serde_json::Value, source_id: &str) -> [i64; 4] {
       if source_id != "claude-code" || value.get("type").and_then(|v| v.as_str()) != Some("assistant") {
           return [0, 0, 0, 0];
       }
       ...
   }
   ```

4. **No FTS.** `subagent_messages` has no `text_content` column (`core/schema.rs:358-373`); `messages` does.

5. **No `RecordSkip` for malformed JSON**, only for typed failures on content-bearing types. `claude/fts_text.rs:157-162`:

   ```rust
   pub fn type_contributes_text(msg_type: &str) -> bool {
       matches!(msg_type, "user" | "assistant" | "system" | "summary" | "ai-title")
   }
   ```
   The filter exists because `{}` lines and skills telemetry live in these files and reporting them would withhold the fingerprint forever (`project_parser.rs:704-708`, test at `:1441-1505`).

6. **Read errors are swallowed entirely** — `let _ = read_jsonl_streaming(...)` (`project_parser.rs:722`). The main path checks `stream_result` and emits a `RecordSkip` on read failure (`project_parser.rs:362-379`).

7. **`message_count` is `transcript.messages.len()`** (`core/writer.rs:1302`) — count of *successfully typed* lines, not lines in the file.

8. **Empty transcripts get a dirty-thread marker** — `core/writer.rs:1372-1387` inserts into `subagent_dirty_threads`.

### Record types that appear

Same enum for both — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/session.rs:23-52`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SessionMessage {
    AgentName(AgentNameMessage),
    Attachment(AttachmentMessage),
    CustomTitle(CustomTitleMessage),
    FileHistorySnapshot(FileHistorySnapshotMessage),
    PrLink(PrLinkMessage),
    Progress(ProgressMessage),
    PermissionMode(PermissionModeMessage),
    #[serde(rename = "saved_hook_context")]
    SavedHookContext(SavedHookContextMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    #[serde(rename = "system")]
    System(SystemMessage),
    #[serde(rename = "summary")]
    Summary(SummaryMessage),
    QueueOperation(QueueOperationMessage),
    LastPrompt(LastPromptMessage),
    AiTitle(AiTitleMessage),
    Mode(ModeMessage),
    BridgeSession(BridgeSessionMessage),
    #[serde(other)]
    Unknown,
}
```

In practice subagent transcripts carry `user` / `assistant` / `system` / `progress`. Two regressions hardened specifically for subagents:

- `AssistantMessage.request_id` made `#[serde(default)]` because "Absent on some API-error assistant lines; a missing `requestId` must not fail the typed parse (which would null FTS + **drop the line from subagent transcripts**)" — `session.rs:355-359`; test `subagent_keeps_assistant_line_without_usage_or_request_id` at `project_parser.rs:1851-1888`.
- `UserMessage.image_paste_ids` widened from `Vec<String>` to `Option<Value>` because the wrong annotation "silently emptied its FTS blob and **dropped it from subagent transcripts**" — `session.rs:291-300`.

### Path B: no difference at all

`adapter.rs:729-754` routes both `PARENT_DECODER` and `SUBAGENT_DECODER` into the **same** `decode_transcript_record`; the only distinction is a guard on `agent_id`:

```rust
PARENT_DECODER => {
    let object_context = ClaudeTranscriptContext::decode(context.object_context)?;
    if object_context.agent_id.is_some() { return Err(decoder_context_mismatch()); }
    decode_transcript_record(...)
}
SUBAGENT_DECODER => {
    let object_context = ClaudeTranscriptContext::decode(context.object_context)?;
    if object_context.agent_id.is_none() { return Err(decoder_context_mismatch()); }
    decode_transcript_record(...)
}
```

Path B keeps raw bytes: `raw_json: record.payload.clone()` (`adapter.rs:2032`). Two branches remain keyed on `agent_id`:

- Session-metadata projection **skipped for subagents** — `adapter.rs:1846-1850`:
  ```rust
  let metadata = if context.agent_id.is_none() {
      session_metadata::project_session_metadata(&value).unwrap_or_default()
  } else {
      session_metadata::SessionMetadataProjection::default()
  };
  ```
- A typed-parse failure emits scoped diagnostic `claude_typed_projection_loss` rather than dropping — `adapter.rs:1969-1992`.

---

## 3. Sidechain handling and parent linkage

### 3.1 `isSidechain` — carried, used for exactly one thing

Modelled in two places:
- `BaseMessageFields.is_sidechain` — `claude/types/session.rs:75-76` (`#[serde(default)] pub is_sidechain: bool`), flattened into `user`/`assistant`/`system`/`attachment`/`progress`/`saved_hook_context`.
- `SessionIndexEntry.is_sidechain` — `claude/types/project.rs:55-56`.

**The only place it changes behaviour in this crate** is first-prompt selection — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/session_metadata.rs:57-75`:

```rust
pub fn extract_human_prompt(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) != Some("user")
        || record.get("isMeta").and_then(Value::as_bool) == Some(true)
        || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        || record.get("isVisibleInTranscriptOnly").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let text = first_text_content(record)?;
    if is_synthetic_prompt_text(&text) { return None; }
    Some(crate::core::text::truncate_utf16(&text, 200).to_owned())
}
```

A sidechain user line can never become a session's `first_prompt` / title.

Everywhere else `is_sidechain` is only **stored and projected**, never used to group or filter:
- `sessions.is_sidechain` — `core/schema.rs:279`, written at `core/writer.rs:1215`.
- `session_index_entry_assertions.is_sidechain` / `canonical_session_index_entries.is_sidechain` — `core/schema.rs:878`, `:913`; written at `engine/session_index_projection.rs:249` (`i64::from(entry.is_sidechain)`), carried through the reduction SELECT at `:471-474`.
- Read back for display: `engine/history_query.rs:486`, `:508`, `:587-588`; `engine/detail_query.rs:111`, `:420`, `:494`; `engine/query_pool.rs:260`; `adapter/facts.rs:343`.
- Codex and Grok readers hard-code `is_sidechain: false` (`codex/reader.rs:464`, `grok/reader.rs:377`).

**There is no "group the sidechain lines of the main transcript into a subagent thread" logic in this crate.** Sidechain grouping is not how subagents are reconstructed — the filesystem layout is (`subagents/agent-*.jsonl`), plus the rules below.

### 3.2 Linking a subagent to its parent — Path A: content-match on `tool_result`

`core/writer.rs:1090-1149`:

```rust
/// Find the `tool_use_id` of the tool call that spawned a subagent.
///
/// Port of the TS `resolveSubagentSpawnToolId`. Claude records a subagent's
/// result as a `tool_result` block in the parent session whose content
/// mentions the agent id; that block's `tool_use_id` is the spawning call.
/// Without this the whole `subagents.spawn_tool_id` / `link_method` pair was
/// written as `NULL` / `"unlinked"` literals — on a real corpus that silently
/// dropped the linkage for 113 of 638 subagents (RFC 008 Phase 5).
fn resolve_spawn_tool_id(
    conn: &Connection, source_id: &str, session_id: &str, agent_id: &str,
) -> Result<Option<String>, WriterError> {
    if agent_id.is_empty() { return Ok(None); }
    let mut stmt = conn.prepare_cached(
        "SELECT data FROM messages WHERE session_id = ?1 AND source_id = ?2 AND data LIKE '%' || ?3 || '%' ORDER BY msg_index",
    )?;
    let mut rows = stmt.query(params![session_id, source_id, agent_id])?;
    while let Some(row) = rows.next()? {
        let data: String = row.get(0)?;
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(&data) else { continue; };
        let Some(content) = raw.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array())
        else { continue; };
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") { continue; }
            let Some(tool_use_id) = block.get("tool_use_id").and_then(|t| t.as_str()) else { continue; };
            let body = match block.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            if body.contains(agent_id) { return Ok(Some(tool_use_id.to_owned())); }
        }
    }
    Ok(None)
}
```

Consumed at `core/writer.rs:1309-1320`:

```rust
// Resolved here rather than in the parser: the linkage lives in
// the *parent session's* messages, which are already written by
// the time a subagent event arrives ...
let spawn_tool_id = resolve_spawn_tool_id(conn, source_id, session_id, &transcript.agent_id)?;
let link_method = if spawn_tool_id.is_some() { "tool_result" } else { "unlinked" };
```

Path A's parent link: **`agent_id` (from filename) → substring search of parent-session `tool_result` bodies → `tool_use_id`**, stored as `subagents.spawn_tool_id` with `link_method ∈ {"tool_result", "unlinked"}` (`core/schema.rs:337-356`). The `LIKE` is only a prefilter, "an optimisation, not the test" (`core/writer.rs:1099-1103`).

**`sourceToolUseID` / `sourceToolAssistantUUID` are NOT used for this** — see §3.4.

### 3.3 Linking a subagent to its parent — Path B: layout + tool-call facts

Run identity composed at `adapter.rs:1093-1103`:

```rust
fn run_native_key(&self) -> String {
    match &self.agent_id {
        Some(agent_id) => format!(
            "{}\0{}\0{}",
            self.session_id,
            self.workflow_id.as_deref().unwrap_or(""),
            agent_id
        ),
        None => self.session_id.clone(),
    }
}
```

`sessionId \0 workflowId \0 agentId`, empty middle segment for non-workflow subagents; a root run's key is just the sessionId.

Parent run derived purely from presence of `agent_id` — `adapter.rs:1890-1899`:

```rust
let parent_run = if context.agent_id.is_some() {
    Some(EntityKey::native(adapter_id, record.source_instance_id, "run",
        context.session_id.as_bytes())?)
} else { None };
```

Role assignment — `adapter.rs:1911-1935`:

```rust
let parent_actor_run = context.agent_id.as_ref()
    .map(|_| output.canonical_root_actor_run_key(context.session_id.as_bytes(), None))
    .transpose()?;
output.push_native(record, run_native_key.as_bytes(),
    Fact::ActorRunRevision(ActorRunRevisionFact {
        actor_run, session: canonical_session,
        role: if context.agent_id.is_some() { ActorRunRole::Child } else { ActorRunRole::Root },
        parent_actor_run,
        native_session_id: Some(context.session_id.clone()),
        native_actor_id: context.agent_id.clone(),
        native_actor_type: None,
    }))?;
```

Delegation fact — `adapter.rs:1938-1959`:

```rust
if let Some(agent_id) = &context.agent_id {
    if !state.delegation_declared || cwd_changed {
        output.push(record, Fact::Delegation(DelegationFact {
            child_run: run.clone(),
            parent_run: delegation_parent,
            session: session.clone(),
            kind: DelegationKind::VendorNativeSubagent,
            relation_strength: RelationStrength::Layout,   // <-- from directory structure
            native_child_id: Some(agent_id.clone()),
            native_task_id: None,
            ...
        }))?;
```

`native_task_id: None` here — the tool-call side comes separately, from **`Task` / `Agent` tool_use blocks in the parent transcript** — `adapter.rs:2488-2514`:

```rust
fn delegation_spawn_descriptors(content: &[ContentBlock]) -> Vec<DelegationSpawnDescriptor> {
    content.iter().filter_map(|block| {
        let ContentBlock::ToolCall { native_id, name, input } = block else { return None; };
        if !matches!(name.as_str(), "Task" | "Agent") || native_id.trim().is_empty() { return None; }
        Some(DelegationSpawnDescriptor {
            native_task_id: native_id.clone(),
            tool_name: name.clone(),
            label: nonempty_field(input, "description").or_else(|| nonempty_field(input, "name")),
            prompt: nonempty_field(input, "prompt"),
            requested_agent_type: nonempty_field(input, "subagent_type")
                .or_else(|| nonempty_field(input, "agent_type")),
        })
    }).collect()
}
```

emitted as `Fact::DelegationSpawn` keyed `run_native_key ‖ native_task_id` (`adapter.rs:2057-2081`).

The **actual join** between "a spawn happened" and "this transcript is that child" is closed by the meta sidecar's `toolUseId` — `adapter.rs:1763-1776`:

```rust
output.push(record, Fact::DelegationMetadata(DelegationMetadataFact {
    child_run,
    session,
    native_child_id: agent_id.to_string(),
    agent_type,
    description: document.description.as_deref().and_then(nonempty),
    name: native_member_name.clone(),
    spawn_depth: document.spawn_depth,
    worktree_path: document.worktree_path.as_deref().and_then(nonempty),
    native_task_id: document.tool_use_id.as_deref().and_then(nonempty),   // <-- the join key
}))?;
```

### 3.4 Field-by-field verdict

| Field | Where modelled | How actually used |
|---|---|---|
| `agentId` (filename) | `project_parser.rs:860-868`; `adapter.rs:1071-1077` | **Primary child identity.** Path A: `subagents.agent_id` + substring probe against parent tool_results. Path B: third segment of `run_native_key`, `ActorRunRevisionFact.native_actor_id`, `DelegationFact.native_child_id`. |
| `agentId` (inside a record) | `UserMessage.agent_id` `session.rs:277-278`; `AssistantMessage.agent_id` `session.rs:361-362`; `ProgressMessage.agent_id` `session.rs:431-432`; `AgentProgress.agent_id` `session.rs:478-479` | **Deserialized but never read** in this crate. |
| `agentId` (journal) | `ClaudeWorkflowJournalDocument.agent_id` `sidecars.rs:214` | **Load-bearing** — required non-empty (`sidecars.rs:1100-1107`); forms `child_run_native_key` (`sidecars.rs:1150-1153`) and `WorkflowMemberEventFact.native_agent_id`. |
| `sourceToolUseID` | `UserMessage.source_tool_use_id`, `#[serde(rename = "sourceToolUseID")]` — `session.rs:271-276` | **Declared only.** Zero read sites (grep-verified). Round-trips through re-serialization. |
| `sourceToolAssistantUUID` | `UserMessage.source_tool_assistant_uuid` — `session.rs:265-270` | **Declared only.** Zero read sites. |
| `parentToolUseID` | `ProgressMessage.parent_tool_use_id` — `session.rs:429-430` | **Declared only.** Zero read sites. |
| `parentUuid` | `BaseMessageFields.parent_uuid` — `session.rs:65-66`; loose read at `adapter.rs:1983` | Path B: becomes `MessageFact.parent_native_message_id` (`adapter.rs:2029`) — intra-transcript threading, **not** cross-agent linkage. Path A: inside `raw_json` only. |
| `workflowId` | dir name `project_parser.rs:678-680`; `adapter.rs:1078-1080` | Path A: `SubagentTranscript.workflow_id`, part of uniqueness key (`core/schema.rs:355`, `:372`). Path B: middle segment of `run_native_key`. |
| `spawnDepth` | `SubagentMeta.spawn_depth: Option<f64>` `project.rs:103`; `ClaudeSubagentMetadataDocument.spawn_depth: Option<u32>` `sidecars.rs:24`; `DelegationMetadataFact.spawn_depth` `adapter/facts.rs:2063` | Path A: **read but never written** (writer pulls only `agent_type` and `worktree_path` — `core/writer.rs:1296-1308`). Path B: projected to `spawn_depth` column (`core/schema.rs:1219`, `:1284`; `engine/projection.rs:2697`, `:2727`, `:2892`), surfaced in orchestration/timeline/search queries (`engine/orchestration_query.rs:87`, `:571`, `:1019`; `engine/timeline_query.rs:1398`). |
| `isSidechain` | see §3.1 | Only filters `extract_human_prompt`; otherwise stored/projected. |

---

## 4. `SubagentType` and where nested/workflow subagents differ

### The enum

`/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/project.rs:63-71`:

```rust
/// TS `SubagentType = 'task' | 'prompt_suggestion' | 'compact'`. Kept as a
/// string-backed enum so serde can deserialize the literal values directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentType {
    Task,
    PromptSuggestion,
    Compact,
}
```

### Inference is purely a filename substring test

`project_parser.rs:870-884`:

```rust
/// TS:
/// ```ts
/// if (fileName.includes('prompt_suggestion')) return 'prompt_suggestion';
/// if (fileName.includes('compact')) return 'compact';
/// return 'task';
/// ```
fn infer_agent_type(file_name: &str) -> SubagentType {
    if file_name.contains("prompt_suggestion") {
        SubagentType::PromptSuggestion
    } else if file_name.contains("compact") {
        SubagentType::Compact
    } else {
        SubagentType::Task
    }
}
```

Order matters (`prompt_suggestion` wins), it is `contains` not prefix/suffix, and it runs on the whole file name including `agent-` and `.jsonl`.

### The enum is a fallback, not the stored value

`core/writer.rs:1292-1301`:

```rust
// Prefer the sidecar's real agent type (general-purpose, Explore, …)
// over the filename-inferred enum kind. `to_string` on the enum
// produces `"task"` (with quotes) — strip them to store the bare
// string, matching the TS convention.
let agent_type = match transcript.meta.as_ref() {
    Some(meta) => meta.agent_type.clone(),
    None => serde_json::to_string(&transcript.agent_type)?
        .trim_matches('"')
        .to_string(),
};
```

`subagents.agent_type` is the sidecar's free-form `agentType` (`"Explore"`, `"general-purpose"`, …) whenever the sidecar exists and parses; the three-valued enum surfaces only when it does not.

Path B has no equivalent enum — it requires a non-empty `agentType` from the sidecar or preserves the record as unknown (`adapter.rs:1736-1744`).

### Where workflow/nested subagents differ

| Aspect | Flat subagent | Workflow-nested subagent |
|---|---|---|
| Location | `<sid>/subagents/agent-*.jsonl` | `<sid>/subagents/workflows/<wf>/agent-*.jsonl` |
| Path A filename filter | any `*.jsonl` (`project_parser.rs:661`) | must start `agent-` (`:688`) |
| Path A depth | flat only | one level only |
| `workflow_id` | `""` (`project_parser.rs:664`; `types/project.rs:86-89`: *"the `wf_...` run this transcript belongs to, or `""` for a top-level (non-workflow) subagent"*) | directory name (`:691`) |
| `run_native_key` (Path B) | `sid \0 "" \0 agentId` | `sid \0 wf_x \0 agentId` |
| Sidecar `description` | usually present | **absent** — `types/project.rs:97`: *"Absent on nested workflow metas — must be optional or their parse fails."* |
| Extra evidence | none | `journal.jsonl` `started`/`result` → `Fact::WorkflowMemberEvent` (`sidecars.rs:1069-1219`) |
| Workflow id validation | n/a | Path A none; Path B requires `wf_` + non-empty suffix (`adapter.rs:1566-1574`) |
| Affiliation | none | `ActorAffiliationRevisionFact` with `ActorAffiliationDimension::Workflow` (`sidecars.rs:1202-1217`) |
| Uniqueness | `UNIQUE(source_id, project_slug, session_id, workflow_id, agent_id)` — `core/schema.rs:355` — so the same `agent_id` can legitimately appear once flat and once per workflow |

`WorkflowRun` doc header, `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/workflow.rs:1-6`:

> One agent-orchestration run at `projects/{slug}/{sessionId}/workflows/{runId}.json`. Session-scoped; its nested subagent transcripts group to it via `SubagentTranscript.workflow_id`.

Workflow journal event kinds — `sidecars.rs:1116-1145`: exactly `"started"` (must have **no** `result` key) and `"result"` (must have one); any mismatch or other `type` is a contract loss. Reduced into `member_status ∈ {result_observed, started, orphan_result}` at `engine/workflow_projection.rs:472-481`.

Reconciliation between the run doc's `agentCount` and observed journal members — `engine/workflow_projection.rs:634-641`:

```rust
let membership_count_status = match (decisive_snapshot, members.observed_count) {
    (_, 0) => "unobserved",
    (None, _) => "snapshot_missing",
    (Some(snapshot), count) if snapshot.agent_count == sqlite_usize(count, "member count")? => "matched",
    (Some(_), _) => "different",
};
```

plus the workflow-vs-member identity join check at `:607-621`. Terminal workflow state is deliberately never propagated to child runs (`workflow_projection.rs:5-7`).

---

## 5. How `sessions-index.json` is read and reconciled with actual JSONL

### Path A — read, tolerate, merge, re-serialize

**Read** — `project_parser.rs:489-509`:

```rust
fn read_sessions_index(project_dir: &Path) -> SessionsIndex {
    let path = project_dir.join("sessions-index.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return empty_sessions_index();
    };
    serde_json::from_str::<SessionsIndex>(&raw).unwrap_or_else(|_| empty_sessions_index())
}
```

Missing **and** malformed both degrade to `SessionsIndex { version: 1, original_path: None, entries: vec![] }` (`:503-509`).

**Merge** — `project_parser.rs:511-535`:

```rust
fn merge_with_discovered_entries(
    index_entries: Vec<SessionIndexEntry>,
    project_dir: &Path,
    original_path: Option<&str>,
) -> Result<Vec<SessionIndexEntry>, ParseError> {
    let mut indexed: HashSet<String> = index_entries.iter().map(|e| e.session_id.clone()).collect();
    let mut merged = index_entries;

    // Sorted, because the source is a directory listing and neither engine
    // gets a guaranteed order from one. NTFS returns entries sorted while ext4
    // and APFS do not, so an unsorted merge agreed on Windows and disagreed on
    // Linux and macOS ... (RFC 008 Phase 5).
    let mut discovered = discover_session_entries(project_dir, original_path)?;
    discovered.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    for entry in discovered {
        if indexed.insert(entry.session_id.clone()) {
            merged.push(entry);
        }
    }
    Ok(merged)
}
```

Index entries win; discovered-only entries are appended, sorted, after them. Nothing is ever removed for being index-only-with-no-file.

**Discover** — `project_parser.rs:545-636`. Directory-enumeration failure is **project-fatal**, not a skipped record:

```rust
// If it fails we cannot know, and a merged index built from the
// sessions-index alone would look complete while silently omitting every
// session on disk — so this is fatal to the project rather than a skipped
// record (RFC 008 Phase 2).
//
// A missing directory is not a failure: the slug came from a directory
// listing, so this means it was deleted mid-run ...
let read_dir = match std::fs::read_dir(project_dir) {
    Ok(rd) => rd,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(e) => return Err(ParseError::Fatal { path: ..., message: e.to_string() }),
};
```

Stub entries filled from file mtime with an explicitly Node-compatible ms computation (`project_parser.rs:596-606`):

```rust
// Node computes `stats.mtimeMs` as `sec * 1000 + nsec / 1e6`. Computing
// `as_secs_f64() * 1000.0` instead rounds differently in the last
// millisecond ... a visible 1 ms disagreement on session timestamps (RFC 008 Phase 5).
.map(|d| d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1_000_000.0)
```

plus `first_prompt` from `peek_first_user_prompt` (`:616`, impl `:1199-1215`, truncated to 200 UTF-16 units) defaulting to `"No prompt"`, and `summary: Some(String::new())` — deliberately `Some("")` not `None`, because "The real corpus has both — 6 NULL and 117 empty" (`types/project.rs:33-42`).

**Persist the MERGED index** — `project_parser.rs:178-189`:

```rust
// 3. Emit the Project event storing the serialized MERGED index
//    (parity with TS `onProject`, which stores
//    `JSON.stringify(parseSessionsIndex(...))`). Storing the raw file
//    text here would drop discovered-but-unindexed sessions from
//    `projects.sessions_index`.
let sessions_index_json = serde_json::to_string(&merged_index).unwrap_or_else(|_| "{}".to_owned());
```

Test `no_sessions_index_stores_merged_discovered_entries` at `project_parser.rs:1735-1777` pins this.

**Per-session file resolution** — `project_parser.rs:290-299`:

```rust
// Canonical path, with fallback to entry.full_path if the canonical
// file doesn't exist (handles stale indices pointing at relocated
// JSONL files). Port of the TS `filePath` ternary.
let canonical_path = project_dir.join(format!("{session_id}.jsonl"));
let file_path: PathBuf = if canonical_path.exists() {
    canonical_path.clone()
} else if !entry.full_path.is_empty() && Path::new(&entry.full_path).exists() {
    PathBuf::from(&entry.full_path)
} else {
    canonical_path.clone()
};
```

An index entry with no file on disk still emits `IngestEvent::Session` + `SessionComplete` with `message_count = 0` — sessions are never dropped for lack of a transcript.

`original_path`: `sessions_index.original_path` if present, else `slug_to_path(slug)` (`project_parser.rs:161-164`); the same fallback applied per-entry inside discovery (`:575-582`).

### Path B — index is Supplemental, never authoritative

`adapter.rs:557-573` declares `SESSION_INDEX_STREAM` as `StreamAuthority::Supplemental`, `EntityScope::Project`, `ConsistencyPolicy::SnapshotReplace`, cap 1 MiB (`adapter.rs:165`).

Decoding is **strict** — `sidecars.rs:1221-1343`. Any of these preserves the whole document as a contract loss rather than partially applying it: not JSON; wrong shape; `version != 1`; `> 4096` entries (`SESSION_INDEX_ENTRY_LIMIT`, `adapter.rs:173`); blank `originalPath`; non-UUID `sessionId`; duplicate `sessionId`; an entry with empty `fullPath` / `firstPrompt` / `created` / `modified` / `projectPath` (`sidecars.rs:1290-1304`).

The projection is where reconciliation happens — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/engine/session_index_projection.rs`. Module doc, `:1-7`:

> A native index is useful metadata, but it is not transcript evidence. Index entries therefore remain queryable when their JSONL is absent and join to canonical transcript sessions only when an independently committed `SessionFact` exists.

The join is a left-lookup against `canonical_sessions` — `session_index_projection.rs:440-457`:

```rust
let transcript_project = transaction
    .query_row(
        "SELECT project_key FROM canonical_sessions WHERE session_key = ?1",
        [session_key],
        |row| row.get::<_, Vec<u8>>(0),
    )
    .optional()
    .map_err(|error| sqlite_error("join session index entry to transcript", error))?;
let (transcript_status, join_conflict) = match transcript_project {
    Some(project_key) if project_key != *decisive_project_key => ("different_project", true),
    Some(_) => ("present", false),
    None => ("missing", false),
};
let resolution_status = if competing_entry_count > 0 || identity_conflict || join_conflict {
    "conflicting"
} else {
    "resolved"
};
```

Each `canonical_session_index_entries` row carries `transcript_status ∈ {present, missing, different_project}` and `resolution_status ∈ {resolved, conflicting}` plus `assertion_count`, `competing_entry_count`, `identity_conflict`, `join_conflict` (`:458-514`). Project-level snapshots compete by digest, `competing_snapshot_count = distinct digests − 1`, status `conflicting`/`resolved` (`:293-304`). Changes published as `history.session-index.changed`, `diagnostic.history.session-index-conflict`, `history.session-index-entry.changed`, `diagnostic.history.session-index-entry-conflict` (`:531`, `:551`, `:566`, `:589`).

Snapshot ownership matters — `session_index_projection.rs:82-92`:

```rust
// A transcript commit may still change a session-index join, but it does
// not own the index document. Only replace assertions when this object
// supplied (or previously supplied) the replace-document snapshot.
if owns_snapshot { ... DELETE FROM session_index_snapshot_assertions WHERE source_object_id = ?1 ... }
```

### Path B — catalog discovery reconciliation

`/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/catalog_discovery.rs` is the third read of the index, at catalog level. Header states the filesystem-is-truth stance (`:6-24`), including:

> Nested `subagents/agent-*.jsonl` transcripts belong to their parent session and are deliberately not catalog members.

Scan depth-pinned to 2 (`:70-76`) with a selector that includes session directories but never recurses into them — `catalog_discovery.rs:250-270`:

```rust
/// Depth-2 selection: project directories and, inside each, the transcripts,
/// session directories, and session index. Session directories are recorded
/// but never descended into, so a deep `subagents/` or `workflows/` tree
/// costs nothing.
fn catalog_selection(path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    ...
    match (kind, components.len()) {
        (DirectoryEntryKind::Directory, 1) => DirectorySelection::Recurse,
        (DirectoryEntryKind::Directory, 2) if is_uuid(&components[1]) => DirectorySelection::Include,
        (DirectoryEntryKind::File, 2) => {
            let name = &components[1];
            if name == SESSION_INDEX_NAME || name.ends_with(".jsonl") {
                DirectorySelection::Include
            } else { DirectorySelection::Ignore }
        }
        _ => DirectorySelection::Ignore,
    }
}
```

Three evidence classes with a precedence order:
- **Session directory** `<slug>/<uuid>/` → `ProjectAssociationBasis::SessionDirectory`, `AssociationQuality::Exact`, `transcript_present: false` (`:122-132`, `:222-244`).
- **Transcript file** `<slug>/<uuid>.jsonl` → same basis, plus size/mtime and `transcript_present: true` (`:138-166`).
- **Index entry** → `ProjectAssociationBasis::NativeProjectIndex`, `AssociationQuality::NativeClaimed`, `transcript_present: false`.

`merge_index_entry` (`:275-317`) *refines* an existing session (title/created/modified/messageCount fill only if absent) and *introduces* index-only sessions, never overwriting the directory association; a project mismatch produces a `DiscoveredAssociationConflict`. `insert_session` (`:326-364`) merges agreeing evidence (the transcript upgrades the directory row) and records the loser as a conflict when projects disagree, keeping the higher `association_quality`.

Index parse here is lenient (a bad index only sets `degraded_reason`) — `:366-403`, with `title` = `summary` else `firstPrompt` (`:395`).

Fallback for the increasingly common no-index case — `catalog_discovery.rs:186-200`:

```rust
// Current Claude installations commonly omit sessions-index.json. The
// slug cannot be reversed safely because '-' may be either a path
// separator or part of a directory name. Use the catalog contract's one
// bounded record-head read instead ... Index evidence wins when it exists.
for (slug, candidate) in transcript_heads {
    ...
    if project.display_path.is_none() {
        project.display_path = read_transcript_cwd(root, &candidate.relative_path, limits.max_head_bytes);
    }
}
```

`read_transcript_cwd` (`:212-219`) reads a bounded prefix of the **newest** transcript (chosen at `:145-155` by `(modified_ns, relative_path)`) and takes the first record's `cwd`.

---

## 6. Project-slug ↔ cwd path encoding/decoding

There is **no encoder** in this crate (encoding is Claude Code's job); there is one decoder, and one deliberate refusal to decode.

### The decoder: `slug_to_path` — `project_parser.rs:1153-1192`

Two-stage. First `slug_shape` classifies the platform — `project_parser.rs:1094-1131`. Its doc table (`:1076-1093`):

```
/// | cwd                      | slug                     | shape         |
/// |--------------------------|--------------------------|---------------|
/// | `/Users/me/app`          | `-Users-me-app`          | `/`, `/`      |
/// | `D:\Projects\app`        | `D--Projects-app`        | `D:\`, `\`    |
/// | `D:\Projects\app` (old)  | `D:-Projects-app`        | `D:\`, `\`    |
```

> A POSIX cwd is always absolute, so it always yields a leading `-`. That makes "no leading dash, but starts with `<letter>--` or `<letter>:-`" an unambiguous Windows drive marker. ... Detection is platform-independent on purpose — a synced `~/.claude` must decode to the same path text on any host.

```rust
fn slug_shape(slug: &str) -> SlugShape<'_> {
    let bytes = slug.as_bytes();
    // `X--…` or `X:-…` — a Windows drive letter. Length 3 is the bare
    // drive root (`D--` → `D:\`), so `>=` not `>`.
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && (bytes[1] == b':' || bytes[1] == b'-')
        && bytes[2] == b'-'
    {
        let drive = (bytes[0] as char).to_ascii_uppercase();
        return SlugShape { prefix: format!("{drive}:\\"), sep: '\\', rest: &slug[3..] };
    }
    if let Some(rest) = slug.strip_prefix('-') {
        return SlugShape { prefix: "/".to_string(), sep: '/', rest };
    }
    SlugShape { prefix: String::new(), sep: '/', rest: slug }
}
```

Order matters: the drive test runs first, but a leading `-` cannot reach it (`-` is not `is_ascii_alphabetic`), so POSIX always wins for absolute slugs — pinned by `slug_to_path_prefers_posix_when_leading_dash_present` (`:1961-1965`: `"-d--Projects-app"` → `"/d//Projects/app"`).

Second stage is a **filesystem probe**, longest-match-first, because the encoding is lossy — `project_parser.rs:1133-1152`:

> Claude Code encodes project paths into slugs by replacing every path separator with `-`. Reversing that naively (every `-` → separator) corrupts legitimate hyphens ... e.g. `-Users-me-Projects-vibe-ctl` can decode to either `/Users/me/Projects/vibe/ctl` or `/Users/me/Projects/vibe-ctl` depending on what actually exists on disk.
>
> We resolve the ambiguity the same way the TS parser does: probe the filesystem from left to right. At each position, try the longest remaining suffix first ... For slugs whose underlying project dir no longer exists ... we degrade gracefully to the naive one-segment-per-dash mapping — the display path may be wrong but ingest still works.

```rust
let parts: Vec<&str> = rest.split('-').collect();
let mut resolved = String::new();
let mut i = 0;
while i < parts.len() {
    let mut matched = false;
    for end in (i + 1..=parts.len()).rev() {          // longest first
        let candidate_segment = parts[i..end].join("-");
        let probe_path = if resolved.is_empty() {
            format!("{prefix}{candidate_segment}")
        } else {
            format!("{prefix}{resolved}{sep}{candidate_segment}")
        };
        if std::fs::metadata(&probe_path).is_ok() {
            if !resolved.is_empty() { resolved.push(sep); }
            resolved.push_str(&candidate_segment);
            i = end; matched = true; break;
        }
    }
    if !matched {
        if !resolved.is_empty() { resolved.push(sep); }
        resolved.push_str(parts[i]);
        i += 1;
    }
}
format!("{prefix}{resolved}")
```

**The probe is greedy per-position with no backtracking** — once a segment is accepted it is never revisited, so a wrong-but-existing longest match can strand the remainder into the naive fallback.

Callers: `project_parser.rs:164` (project `original_path` when the index has none) and `:578` (per-entry `project_path` fallback, derived from `project_dir.file_name()`).

Tests: `:1931-1984` — POSIX (`"-"` → `"/"`, `""` → `""`), Windows both spellings, uppercase normalization of the drive letter (`"d--Projects-app"` → `"D:\\Projects\\app"`), bare roots, and a live-filesystem probe test proving `p100-app` survives as one segment (`:1971-1984`).

### The refusal

`catalog_discovery.rs:186-191` deliberately does **not** call any decoder:

> The slug cannot be reversed safely because '-' may be either a path separator or part of a directory name. Use the catalog contract's one bounded record-head read instead: it stays confined to the authorized projects root and gives the same cwd evidence the full decoder will later commit. Index evidence wins when it exists.

Precedence for `DiscoveredProject.display_path`: `sessions-index.json` `entries[].projectPath` (`catalog_discovery.rs:168-184`), else the newest transcript's `cwd` (`:192-200`), else `None`. The slug is never reversed.

The adapter/fact path likewise never decodes: `native_project_key` **is** the slug verbatim (`adapter.rs:1878`, `catalog_discovery.rs:113-117`, and `catalog_discovery.rs:20-22`: *"`<project-slug>` is byte-identical to the `native_project_key` the Claude decoder derives from the same path"*). Path B gets cwd from the record itself — `adapter.rs:1851` `let cwd = nonempty_field(&value, "cwd");` → `SessionFact.cwd` (`:1879`), with change detection so a moved workspace re-declares (`:1853-1854`, `:1864`).

Project enumeration is just a directory listing — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/orchestrate/ingest.rs:1043-1064`:

```rust
/// List immediate subdirectories of `<agent_dir>/projects/`. Each dir
/// name is a project slug. Non-directory entries (e.g. `.DS_Store`) are
/// skipped silently.
fn scan_project_slugs(agent_dir: &Path) -> std::result::Result<Vec<String>, std::io::Error> { ... slugs.sort(); ... }
```

Easy to confuse but unrelated: `platform_path_key` (`/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/source/file.rs:465-473`) is a length-prefixed byte encoding of a path used as a *source-instance identity key* — not a slug codec.

---

## 7. Divergences and sharp edges worth flagging

1. **`^agent-(a.+)\.jsonl$` vs `^agent-(.+)\.jsonl$` vs `strip_prefix("agent-")`.** `project_parser.rs:50` requires the agent id to begin with `a`; `fingerprint.rs:94` and `adapter.rs:1072` do not. An id not starting with `a` gets `agent_id = "agent-<id>"` in Path A and `"<id>"` in Path B, and the Path A value then fails to substring-match the `tool_result` body in `resolve_spawn_tool_id` (`core/writer.rs:1143`), silently downgrading `link_method` to `"unlinked"`.
2. **Path A's nested walk is one level deep** (`project_parser.rs:682-694`) while the declared glob and Path B allow `subagents/workflows/*/**/agent-*.jsonl` (`adapter.rs:354`, fixture `adapter.rs:4058`). Path A misses `.../wf_main/agents/agent-child.jsonl`.
3. **Path A's flat walk accepts any `*.jsonl`** under `subagents/` (`project_parser.rs:661`), so a stray non-`agent-` file becomes a `SubagentTranscript` whose `agent_id` is the bare stem.
4. **Subagent messages are re-serialized, not stored verbatim** (`core/writer.rs:1348`, `:1368`) — unmodelled JSON keys are lost except where a `#[serde(flatten)] extra` map catches them.
5. **`sourceToolUseID`, `sourceToolAssistantUUID`, `parentToolUseID` are dead weight** in this crate — declared for shape fidelity, read nowhere. The parent link is filename-`agentId` + `tool_result` substring (Path A) or directory layout + `meta.json` `toolUseId` (Path B). If someone expects these fields to be the join key, they are not.
6. **`spawnDepth` is parsed but discarded on Path A** — `SubagentMeta.spawn_depth` (`types/project.rs:103`) is never read by `core/writer.rs`; only Path B's `DelegationMetadataFact` persists it.
7. **`workflow_id` is unvalidated on Path A** (any directory name under `workflows/`), strictly `wf_`-prefixed on Path B (`adapter.rs:1566-1574`); and Path A takes the run id from the JSON body's `runId` while Path B rejects a `runId`/filename mismatch (`sidecars.rs:994-1001`).
8. **`isSidechain` does no grouping anywhere in this crate.** Its only behavioural effect is disqualifying a line from becoming `first_prompt` (`session_metadata.rs:60`).
The report is complete and was delivered in full in my previous message. Nothing further is pending.

To summarize the essentials for relay:

**Two parallel ingest paths exist in the crate** — the legacy `project_parser.rs` stream parser (ported from `packages/sdk/src/parser/project-parser.ts`) and the newer declarative adapter in `claude/adapter.rs`. They disagree in several load-bearing places, which is the single most important finding.

**Key answers:**

1. **Globs** — authoritative strings live in `adapter.rs:353-354` (`*/*/subagents/**/agent-*.jsonl`, `*/*/subagents/workflows/*/**/agent-*.jsonl`), `:374` (meta), `:544` (journal), `:527` (`*/*/workflows/wf_*.json`), `:563` (sessions-index). `agentId` comes from the filename via `^agent-(a.+)\.jsonl$` (Path A, `project_parser.rs:50`) vs plain `strip_prefix` (Path B, `adapter.rs:1072`); `sessionId`/`workflowId` come from path components (`adapter.rs:1062-1087`).

2. **Subagent vs main parsing** — same `SessionMessage` enum, but subagent lines are typed-parse-only (no `raw_json` fallback — a failed line is permanently lost, `project_parser.rs:715-745`) and are **re-serialized** through serde before storage (`core/writer.rs:1348`), unlike main transcript lines which are stored verbatim.

3. **Sidechain/linkage** — `isSidechain` does *no* grouping; its only effect is disqualifying a line from becoming `first_prompt` (`session_metadata.rs:60`). `sourceToolUseID`, `sourceToolAssistantUUID`, `parentToolUseID` are declared but **never read**. Parent linkage is: Path A = filename `agentId` substring-matched against parent `tool_result` bodies (`core/writer.rs:1104-1149`); Path B = directory layout (`RelationStrength::Layout`) plus the sidecar's `toolUseId` as the join key (`adapter.rs:1774`).

4. **SubagentType** — filename `contains` test only (`project_parser.rs:876-884`), and it's a *fallback*: the sidecar's free-form `agentType` wins when present (`core/writer.rs:1296-1301`).

5. **sessions-index** — Path A merges index entries with disk-discovered JSONL and stores the *merged* result; Path B treats the index as `Supplemental` and joins to `canonical_sessions`, emitting `transcript_status ∈ {present, missing, different_project}`.

6. **Slug decoding** — `slug_to_path` (`project_parser.rs:1153-1192`) probes the filesystem longest-match-first to disambiguate hyphens; the catalog path deliberately refuses to decode and reads `cwd` from the newest transcript head instead (`catalog_discovery.rs:186-200`).

**Eight divergences/sharp edges** are enumerated in §7 of the report — the most consequential being the `a`-prefix regex constraint (silently breaks spawn linkage for non-`a` agent ids) and Path A's one-level-deep nested workflow walk (misses transcripts the declared glob covers).
The investigation is complete. The full structured report was delivered inline in my prior message; here is the essential summary for relay.

## What was done

Read `project_parser.rs` in full (2023 lines, two passes due to token cap), `catalog_discovery.rs` (437), `session_index_projection.rs` (729), and targeted regions of `workflow_projection.rs`. Supporting reads that turned out to be load-bearing: `claude/adapter.rs` (7420 lines, targeted — this holds the actual glob strings), `claude/adapter/sidecars.rs`, `claude/types/session.rs`, `claude/types/project.rs`, `claude/fingerprint.rs`, `core/writer.rs`, `claude/message_extractor.rs`, `claude/session_metadata.rs`, `claude/fts_text.rs`, `core/schema.rs`.

## Key findings

**Framing that changes every answer: there are two parallel ingest paths.** Path A is the legacy `ProjectParser` (`project_parser.rs:116`, ported from `packages/sdk/src/parser/project-parser.ts`) emitting `IngestEvent`s into `core/writer.rs`. Path B is the declarative adapter (`adapter.rs:328`) emitting `Fact`s into `engine/*_projection.rs`. Both are live and they disagree in several load-bearing places.

1. **Globs** — authoritative strings are in `adapter.rs`: `*/*/subagents/**/agent-*.jsonl` and `*/*/subagents/workflows/*/**/agent-*.jsonl` (`:353-354`), `*/*/subagents/**/agent-*.meta.json` (`:374`), `*/*/subagents/workflows/*/journal.jsonl` (`:544`), `*/*/workflows/wf_*.json` (`:527`), `*/sessions-index.json` (`:563`). `agentId` from filename; `sessionId`/`workflowId` from path components (`adapter.rs:1062-1087`).

2. **Subagent vs main parsing** — same `SessionMessage` enum, but subagent lines are typed-parse-only with **no `raw_json` fallback** (a failed line is permanently gone, `project_parser.rs:715-745`) and are **re-serialized through serde** before storage (`core/writer.rs:1348`), unlike main lines stored verbatim. No FTS on subagent messages.

3. **Sidechain/linkage** — `isSidechain` performs **no grouping anywhere**; its only behavioural effect is disqualifying a line from becoming `first_prompt` (`session_metadata.rs:60`). `sourceToolUseID`, `sourceToolAssistantUUID`, `parentToolUseID` are declared but **never read** (grep-verified). Real linkage: Path A substring-matches `agentId` against parent `tool_result` bodies (`core/writer.rs:1104-1149`); Path B uses directory layout (`RelationStrength::Layout`) joined via the sidecar's `toolUseId` (`adapter.rs:1774`).

4. **SubagentType** — filename `contains` test only (`project_parser.rs:876-884`), and it is a *fallback*: the sidecar's free-form `agentType` wins when present (`core/writer.rs:1296-1301`).

5. **sessions-index** — Path A merges index entries with disk-discovered JSONL and persists the *merged* index; Path B treats it as `Supplemental` and joins to `canonical_sessions`, emitting `transcript_status ∈ {present, missing, different_project}`.

6. **Slug decoding** — `slug_to_path` (`project_parser.rs:1153-1192`) probes the filesystem longest-match-first to disambiguate hyphens; `catalog_discovery.rs:186-200` deliberately refuses to decode and reads `cwd` from the newest transcript head instead.

**Two divergences worth acting on:** the `^agent-(a.+)\.jsonl$` regex (`project_parser.rs:50`) requires agent ids to start with `a` — otherwise Path A stores `agent_id = "agent-<id>"` and the spawn-linkage substring probe silently fails to `"unlinked"`; and Path A's nested workflow walk is only one level deep (`project_parser.rs:682-694`), missing transcripts the declared `**` glob covers. Six more are enumerated in §7 of the full report.
The task is complete and the full report was delivered inline two messages ago. Repeating the essentials once more for relay:

## Investigation summary

**Files read:** `project_parser.rs` in full (2023 lines, two passes), `catalog_discovery.rs` (437), `session_index_projection.rs` (729), targeted `workflow_projection.rs`. Supporting: `claude/adapter.rs` (targeted — holds the actual glob strings), `claude/adapter/sidecars.rs`, `claude/types/session.rs`, `claude/types/project.rs`, `claude/fingerprint.rs`, `core/writer.rs`, `claude/message_extractor.rs`, `claude/session_metadata.rs`, `claude/fts_text.rs`, `core/schema.rs`.

**Framing that changes every answer:** there are two parallel ingest paths. Path A = legacy `ProjectParser` (`project_parser.rs:116`, ported from `packages/sdk/src/parser/project-parser.ts`) → `IngestEvent` → `core/writer.rs`. Path B = declarative adapter (`adapter.rs:328`) → `Fact` → `engine/*_projection.rs`. Both live; they disagree in load-bearing places.

**The six questions:**

1. **Globs** — authoritative in `adapter.rs`: `*/*/subagents/**/agent-*.jsonl` + `*/*/subagents/workflows/*/**/agent-*.jsonl` (`:353-354`), `*/*/subagents/**/agent-*.meta.json` (`:374`), `*/*/subagents/workflows/*/journal.jsonl` (`:544`), `*/*/workflows/wf_*.json` (`:527`), `*/sessions-index.json` (`:563`). IDs: `agentId` from filename, `sessionId`/`workflowId` from path components (`adapter.rs:1062-1087`).

2. **Subagent vs main parsing** — same `SessionMessage` enum, but subagent lines are typed-parse-only with **no `raw_json` fallback** (failed line permanently lost, `project_parser.rs:715-745`) and are **re-serialized through serde** before storage (`core/writer.rs:1348`). Main lines stored verbatim. No FTS on subagent messages.

3. **Sidechain/linkage** — `isSidechain` does **no grouping anywhere**; sole effect is disqualifying a line from `first_prompt` (`session_metadata.rs:60`). `sourceToolUseID`, `sourceToolAssistantUUID`, `parentToolUseID` are declared but **never read** (grep-verified). Real linkage: Path A substring-matches `agentId` against parent `tool_result` bodies (`core/writer.rs:1104-1149`); Path B uses directory layout (`RelationStrength::Layout`) joined via the sidecar's `toolUseId` (`adapter.rs:1774`).

4. **SubagentType** — filename `contains` test (`project_parser.rs:876-884`), and it's a *fallback*: the sidecar's free-form `agentType` wins (`core/writer.rs:1296-1301`).

5. **sessions-index** — Path A merges index entries with disk-discovered JSONL and persists the *merged* index; Path B treats it as `Supplemental`, joins to `canonical_sessions`, emits `transcript_status ∈ {present, missing, different_project}`.

6. **Slug decoding** — `slug_to_path` (`project_parser.rs:1153-1192`) probes the filesystem longest-match-first to disambiguate hyphens; `catalog_discovery.rs:186-200` deliberately refuses to decode, reading `cwd` from the newest transcript head instead.

**Two divergences worth acting on:** the `^agent-(a.+)\.jsonl$` regex (`project_parser.rs:50`) requires agent ids to start with `a` — otherwise Path A stores `agent_id = "agent-<id>"` and the spawn-linkage probe silently degrades to `"unlinked"`; and Path A's nested workflow walk is only one level deep (`project_parser.rs:682-694`), missing transcripts the declared `**` glob covers. Six further edges are enumerated in §7 of the full report.
# Claude Code subagent-transcript discovery & parsing in `spaghetti-napi`

All paths relative to `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/`. Absolute paths given at first mention.

---

## 0. Orientation — there are TWO parallel ingest paths

This framing changes every answer below. Both are live.

| | **Path A — legacy stream parser** | **Path B — adapter / ADS** |
|---|---|---|
| Entry | `ProjectParser::parse_project` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/project_parser.rs:116` | `ClaudeCodeAdapter::streams` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter.rs:328` |
| Discovery | `std::fs::read_dir` walks, hand-rolled per artifact | Declarative `ObjectSelector { root_name, include: [glob] }` |
| Output | `IngestEvent` over `crossbeam_channel` → `core/writer.rs` → `subagents`, `subagent_messages`, `workflows` | `Fact::*` → `engine/*_projection.rs` → `canonical_*` |
| Warm start | `claude/fingerprint.rs` (`source_files` categories) | catalog + cursors (`claude/catalog_discovery.rs`) |
| Ported from | `packages/sdk/src/parser/project-parser.ts` (comments say so verbatim) | RFC 012A/012C native fact model |

Path A is the PRIMARY FILE. Path B holds the authoritative glob strings.

---

## 1. Path patterns, globs, ID extraction

### 1.1 Declarative globs (Path B)

```rust
// adapter.rs:347-357 — SUBAGENT_STREAM
selector: ObjectSelector {
    root_name: "projects".to_string(),
    include: vec![
        "*/*/subagents/**/agent-*.jsonl".to_string(),
        "*/*/subagents/workflows/*/**/agent-*.jsonl".to_string(),
    ],
    exclude: Vec::new(),
},
```
```rust
// adapter.rs:374 — SUBAGENT_META_STREAM
include: vec!["*/*/subagents/**/agent-*.meta.json".to_string()],
// adapter.rs:544 — WORKFLOW_JOURNAL_STREAM
include: vec!["*/*/subagents/workflows/*/journal.jsonl".to_string()],
```

| Stream | line | root | glob(s) |
|---|---|---|---|
| `PARENT_STREAM` | 335 | projects | `*/*.jsonl` |
| `SUBAGENT_STREAM` | 353-354 | projects | `*/*/subagents/**/agent-*.jsonl`, `*/*/subagents/workflows/*/**/agent-*.jsonl` |
| `SUBAGENT_META_STREAM` | 374 | projects | `*/*/subagents/**/agent-*.meta.json` |
| `WORKFLOW_RUN_STREAM` | 527 | projects | `*/*/workflows/wf_*.json` |
| `WORKFLOW_JOURNAL_STREAM` | 544 | projects | `*/*/subagents/workflows/*/journal.jsonl` |
| `SESSION_INDEX_STREAM` | 563 | projects | `*/sessions-index.json` |
| `PROJECT_MEMORY_STREAM` | 582 | projects | `*/memory/*.md` |
| `PERSISTED_TOOL_RESULT_STREAM` | 601 | projects | `*/*/tool-results/*.txt` |
| `TODO_STREAM` | 451 | home | `todos/*-agent-*.json` |
| `TASK_ITEM_STREAM` | 470 | home | `tasks/*/*.json` |
| `PLAN_STREAM` | 489 | home | `plans/*.md` |
| `ARTIFACT_CONTENT_STREAM` | 508 | home | `file-history/*/*@v*` |
| `TEAM_CONFIG_STREAM` | 393 | teams | `*/config.json` |
| `TEAM_INBOX_STREAM` | 412 | teams | `*/inboxes/*.json` |
| `ACTIVE_SESSION_STREAM` | 432 | sessions | `*.json` |
| `INTERPRETATION_SETTINGS_STREAM` | 620-623 | home | `settings.json`, `settings.local.json` |

Roots at `adapter.rs:296-313`: `home` = `~/.claude`, `projects`/`teams`/`sessions` beneath it.

### 1.2 Imperative walks (Path A) — asymmetric

```rust
// project_parser.rs:647-698
fn read_subagents(project_dir: &Path, session_id: &str)
    -> Vec<(SubagentTranscript, Option<DroppedSubagentLines>)> {
    let dir = project_dir.join(session_id).join("subagents");
    let mut out = Vec::new();

    // Top-level subagent transcripts (not associated with a workflow).
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            ...
            if !name.ends_with(".jsonl") { continue; }   // ANY *.jsonl — no agent- prefix required
            out.push(read_one_subagent(&entry.path(), name, ""));
        }
    }

    // Nested workflow subagent transcripts:
    //   subagents/workflows/{wf_id}/agent-*.jsonl  (journal.jsonl is skipped
    //   by the `agent-` prefix). ...
    if let Ok(wf_dirs) = std::fs::read_dir(dir.join("workflows")) {
        for wf_entry in wf_dirs.flatten() {
            if !wf_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
            let wf_name = wf_entry.file_name();
            let Some(workflow_id) = wf_name.to_str() else { continue; };
            if let Ok(agent_dir) = std::fs::read_dir(wf_entry.path()) {
                for entry in agent_dir.flatten() {
                    ...
                    if !name.starts_with("agent-") || !name.ends_with(".jsonl") { continue; }
                    out.push(read_one_subagent(&entry.path(), name, workflow_id));
                }
            }
        }
    }
    out
}
```

- Top-level (`:661`) accepts **any** `*.jsonl`; nested (`:688`) requires `agent-` — that is what excludes `journal.jsonl`.
- **Nested walk is one level only.** The glob and Path B allow `subagents/workflows/*/**/agent-*.jsonl` (`adapter.rs:354`; fixture `adapter.rs:4058` uses `.../wf_main/agents/agent-child.jsonl`). Path A misses those.
- `workflow_id` = raw directory name, unvalidated.

### 1.3 `agent-{id}.meta.json`

```rust
// project_parser.rs:746-749
// Sibling `agent-{id}.meta.json` carries the real agent type + description.
let meta = std::fs::read_to_string(path.with_extension("meta.json"))
    .ok()
    .and_then(|raw| serde_json::from_str::<SubagentMeta>(&raw).ok());
```
`with_extension` replaces only the final extension → `agent-a1.jsonl` → `agent-a1.meta.json`. Missing/malformed is silently `None`.

`SubagentMeta` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/project.rs:93-106`:
```rust
pub struct SubagentMeta {
    pub agent_type: String,
    // Absent on nested workflow metas — must be optional or their parse fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub name: Option<String>,
    pub spawn_depth: Option<f64>,
    pub worktree_path: Option<String>,
}
```
Path B's richer variant adds `teamName` + `toolUseId` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter/sidecars.rs:13-29`. Real fixture at `adapter.rs:7045`:
`{"agentType":"Explore","description":"map the parser","name":"survey","spawnDepth":2,"worktreePath":"/repo/.worktrees/a1","toolUseId":"tool-7"}`

### 1.4 `agentId` from filename — the divergence

```rust
// project_parser.rs:49-50
/// Matches `agent-<id>.jsonl` where `<id>` starts with `a`. Port of
/// `extractAgentId`'s `^agent-(a.+)\.jsonl$` pattern.
static SUBAGENT_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^agent-(a.+)\.jsonl$").expect("SUBAGENT_FILE regex compiles"));
```
```rust
// project_parser.rs:860-868
fn extract_agent_id(file_name: &str) -> String {
    if let Some(caps) = SUBAGENT_FILE.captures(file_name) {
        caps.get(1).map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| file_name.trim_end_matches(".jsonl").to_owned())
    } else {
        file_name.trim_end_matches(".jsonl").to_owned()
    }
}
```
The `a` is inside the capture, so `agent-abc123.jsonl` → `"abc123"` (asserted `:1435`). But an id not starting with `a` fails the regex and falls back to the **whole stem including `agent-`**. Compare `fingerprint.rs:93-94` (`^agent-(.+)\.jsonl$`) and `adapter.rs:1071-1077`:
```rust
let Some(agent_id) = file_name
    .strip_prefix("agent-")
    .and_then(|name| name.strip_suffix(suffix))
    .filter(|value| !value.is_empty())
else { return Err(path_error(relative_path, "invalid subagent object name")); };
```
`agent-b7.jsonl` → Path A `"agent-b7"`, Path B `"b7"`.

### 1.5 sessionId / workflowId from directories

Path A never parses `session_id` from a subagent path — it is passed in (`:382`). Path B — `adapter.rs:1062-1087`:
```rust
if components.len() < 4 || components.get(2).map(String::as_str) != Some("subagents") { ...err... }
...
let workflow_id = components.windows(2).find_map(|pair| {
    (pair[0] == "workflows" && !pair[1].is_empty()).then(|| pair[1].clone())
});
Ok(Self { project_slug: components[0].clone(), session_id: components[1].clone(),
          agent_id: Some(agent_id.to_string()), workflow_id })
```
Suffix dispatch `.jsonl`/`.meta.json` at `:1054-1060`. Journal path strict-shaped at `adapter.rs:1534-1552` (6 components, `[2]=="subagents"`, `[3]=="workflows"`, `[5]=="journal.jsonl"`); run doc at `:1515-1532`; `validate` requires `is_uuid(session)` + `wf_` prefix (`:1554-1580`).

### 1.6 `workflows/{runId}.json` (Path A)

```rust
// project_parser.rs:776
if !name.starts_with("wf_") || !name.ends_with(".json") { continue; }
// :786-790
let workflow_id = data.get("runId").and_then(|v| v.as_str()).map(str::to_owned)
    .unwrap_or_else(|| name.trim_end_matches(".json").to_owned());
```
Id from the **body's `runId`**, filename as fallback. Path B instead *rejects* on mismatch (`sidecars.rs:994-1001`). `subagent_count` recounted at `:802`/`:823-841`; journal read at `:843-857`; runs sorted by id (`:819`).

### 1.7 Other Path A artifacts

| Artifact | Pattern | Code |
|---|---|---|
| session JSONL | `^[0-9a-f]{8}-...-[0-9a-f]{12}\.jsonl$` | `:42-45` |
| tool results | `*.txt`, id = stem | `:888-913` |
| file history | `^([0-9a-f]+)@v(\d+)$` | `:54-56`, `:917-968` |
| todos | prefix `{sid}-agent-` then `^(.+?)-agent-(.+)\.json$` | `:60-61`, `:972-1013` |
| tasks | `.lock` / `.highwatermark`, `parseInt`-lenient | `:1017-1040` |
| plans | `plans/*.md`, title `(?m)^#\s+(.+)$` | `:1222-1275` |
| memory | `memory/MEMORY.md` | `:640-643` |

Warm-start taxonomy `fingerprint.rs:30-40`; `CATEGORY_SUBAGENT_META` / `CATEGORY_WORKFLOW_JOURNAL` at `:79-82` exist because "a changed agent type, a changed journal, or a whole workflow's transcripts were invisible to warm start — RFC 008 Phase 1.4" (`:608-616`).

---

## 2. Subagent vs main transcript parsing

**Same `SessionMessage` enum — nothing else is shared.**

Main line: `project_parser.rs:309-356` → `build_message_event` (`:458-485`) → `message_extractor::project_jsonl_line` (`message_extractor.rs:50-57`): loose `Value` parse is authoritative for `msg_type`/`uuid`/`timestamp`; typed parse is only for FTS; **raw line stored verbatim** as `raw_json` (`:474`); a typed failure keeps the row and merely reports lost FTS.

Subagent line — `project_parser.rs:715-761`:
```rust
let _ = read_jsonl_streaming(path, 0, |line, idx, _off| {
    match serde_json::from_str::<SessionMessage>(line) {
        Ok(msg) => messages.push(msg),
        Err(e) => {
            let msg_type = serde_json::from_str::<Value>(line).ok()
                .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            if !fts_text::type_contributes_text(&msg_type) { return; }
            ...
        }
    }
});
```

Differences:
1. **Typed parse is the only parse and is load-bearing** — a failure means the line is gone. Doc `:700-708`: *"the transcript is the only thing holding it, so there is no verbatim JSON to fall back on."*
2. **Re-serialized, not verbatim** — `core/writer.rs:1347-1371`: `let value = serde_json::to_value(message)?;` … `serde_json::to_string(&value)?`. Unmodelled keys are lost except via `#[serde(flatten)] extra` (`session.rs:400-403`, `:213-214`). Timestamp re-read from the re-encoded value.
3. Separate token extractor `core/writer.rs:1151-1171`.
4. **No FTS column** on `subagent_messages` (`core/schema.rs:358-373`).
5. Drops reported only for content-bearing types — `fts_text.rs:157-162`: `matches!(msg_type, "user"|"assistant"|"system"|"summary"|"ai-title")`.
6. **Read errors swallowed** (`let _ =`, `:722`) vs checked on the main path (`:362-379`).
7. `message_count` = successfully-typed lines (`core/writer.rs:1302`).
8. Empty transcripts → `subagent_dirty_threads` (`core/writer.rs:1372-1387`).

Enum at `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/session.rs:23-52` (`#[serde(tag="type", rename_all="kebab-case")]`, 17 variants + `#[serde(other)] Unknown`). Two hardenings exist *because* of subagents: `AssistantMessage.request_id` defaulted (`session.rs:355-359`, test `:1851-1888`) and `image_paste_ids` widened to `Option<Value>` (`session.rs:291-300`) — both previously "dropped it from subagent transcripts."

Path B makes no distinction: `adapter.rs:729-754` routes both decoders into the same `decode_transcript_record`, guarding only on `agent_id` presence; raw bytes kept (`:2032`).

---

## 3. Sidechain and parent linkage

### `isSidechain` does no grouping
Modelled at `session.rs:75-76` and `project.rs:55-56`. Its **only** behavioural use — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/session_metadata.rs:57-75`:
```rust
if record.get("type").and_then(Value::as_str) != Some("user")
    || record.get("isMeta").and_then(Value::as_bool) == Some(true)
    || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
    || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
    || record.get("isVisibleInTranscriptOnly").and_then(Value::as_bool) == Some(true)
{ return None; }
```
Elsewhere it is stored/projected only: `core/schema.rs:279`/`:878`/`:913`, `core/writer.rs:1215`, `session_index_projection.rs:249`/`:471-474`, read back in `history_query.rs:486`, `detail_query.rs:494`, `query_pool.rs:260`.

### Path A linkage — substring match on `tool_result`
`core/writer.rs:1090-1149`:
```rust
/// Port of the TS `resolveSubagentSpawnToolId`. Claude records a subagent's
/// result as a `tool_result` block in the parent session whose content
/// mentions the agent id; that block's `tool_use_id` is the spawning call.
/// Without this ... silently dropped the linkage for 113 of 638 subagents (RFC 008 Phase 5).
... "SELECT data FROM messages WHERE session_id = ?1 AND source_id = ?2 AND data LIKE '%' || ?3 || '%' ORDER BY msg_index"
    if body.contains(agent_id) { return Ok(Some(tool_use_id.to_owned())); }
```
Consumed `:1309-1320` → `spawn_tool_id`, `link_method ∈ {"tool_result","unlinked"}`.

### Path B linkage — layout + `toolUseId`
`run_native_key` = `sessionId \0 workflowId \0 agentId` (`adapter.rs:1093-1103`); parent run = bare sessionId when `agent_id.is_some()` (`:1890-1899`); role `Child`/`Root` (`:1925-1929`); `DelegationFact { kind: VendorNativeSubagent, relation_strength: RelationStrength::Layout, native_task_id: None }` (`:1938-1959`). Spawn side from `Task`/`Agent` tool_use blocks (`:2488-2514`, reading `description`/`name`, `prompt`, `subagent_type`/`agent_type`). The join closes via the sidecar: `native_task_id: document.tool_use_id...` (`adapter.rs:1774`).

### Field verdict
| Field | Modelled | Used? |
|---|---|---|
| `agentId` (filename) | `project_parser.rs:860-868`; `adapter.rs:1071-1077` | **Primary child identity** both paths |
| `agentId` (in-record) | `session.rs:277-278`, `:361-362`, `:431-432`, `:478-479` | **Never read** |
| `sourceToolUseID` | `session.rs:271-276` | **Never read** |
| `sourceToolAssistantUUID` | `session.rs:265-270` | **Never read** |
| `parentToolUseID` | `session.rs:429-430` | **Never read** |
| `parentUuid` | `session.rs:65-66`; `adapter.rs:1983` | Path B → `MessageFact.parent_native_message_id` (`:2029`) — intra-transcript only |
| `workflowId` | `project_parser.rs:678-680`; `adapter.rs:1078-1080` | Uniqueness key (`schema.rs:355`); middle key segment |
| `spawnDepth` | `project.rs:103`; `sidecars.rs:24`; `facts.rs:2063` | Path A **discarded**; Path B projected (`schema.rs:1219`, `projection.rs:2727`, `orchestration_query.rs:1019`) |
| `isSidechain` | above | first_prompt filter only |

---

## 4. `SubagentType` + nested/workflow differences

```rust
// types/project.rs:63-71
/// TS `SubagentType = 'task' | 'prompt_suggestion' | 'compact'`.
#[serde(rename_all = "snake_case")]
pub enum SubagentType { Task, PromptSuggestion, Compact }
```
```rust
// project_parser.rs:876-884
fn infer_agent_type(file_name: &str) -> SubagentType {
    if file_name.contains("prompt_suggestion") { SubagentType::PromptSuggestion }
    else if file_name.contains("compact") { SubagentType::Compact }
    else { SubagentType::Task }
}
```
Order-dependent, `contains` (not prefix/suffix), run over the whole filename. **It is only a fallback** — `core/writer.rs:1292-1301`: the sidecar's free-form `agentType` (`"Explore"`, `"general-purpose"`) wins; the enum is serialized and de-quoted only when no sidecar parses. Path B has no such enum (`adapter.rs:1736-1744` requires a non-empty `agentType` or preserves unknown).

| Aspect | Flat | Workflow-nested |
|---|---|---|
| Path A filter | any `*.jsonl` (`:661`) | `agent-` required (`:688`) |
| Depth | flat | one level only |
| `workflow_id` | `""` (`:664`; `project.rs:86-89`) | dir name (`:691`) |
| `run_native_key` | `sid \0 "" \0 agentId` | `sid \0 wf_x \0 agentId` |
| Sidecar `description` | present | **absent** (`project.rs:97`) |
| Extra evidence | none | `journal.jsonl` → `WorkflowMemberEvent` (`sidecars.rs:1069-1219`) |
| wf id validation | n/a | Path A none; Path B `wf_`+suffix (`adapter.rs:1566-1574`) |

Journal kinds strictly `started` (no `result`) / `result` (`sidecars.rs:1116-1145`) → `member_status ∈ {result_observed, started, orphan_result}` (`workflow_projection.rs:472-481`). Count reconciliation `workflow_projection.rs:634-641` → `{unobserved, snapshot_missing, matched, different}`; join check `:607-621`. Terminal workflow state never propagated to children (`:5-7`).

---

## 5. sessions-index.json read + reconciliation

**Path A.** Read (`:489-509`) — missing *and* malformed both degrade to an empty synthetic index. Merge (`:511-535`) — index entries win, discovered entries appended **sorted** ("NTFS returns entries sorted while ext4 and APFS do not… RFC 008 Phase 5"). Discovery (`:545-636`) — `read_dir` failure is **project-fatal** (`:558-573`), `NotFound` is not; mtime computed Node-style (`:596-606`); `first_prompt` peeked (`:616`, `:1199-1215`, 200 UTF-16 units); `summary: Some("")` deliberately (`project.rs:33-42`, "6 NULL and 117 empty"). The **merged** index is persisted (`:178-189`), test `:1735-1777`. Per-session file resolution falls back to `entry.full_path` (`:290-299`); an index entry with no file still emits `Session` + `SessionComplete` with count 0.

**Path B.** `Supplemental` authority (`adapter.rs:567`), strict decode (`sidecars.rs:1221-1343`) — any of version≠1, >4096 entries, non-UUID/duplicate `sessionId`, blank required field preserves the whole doc as contract loss. Reconciliation in `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/engine/session_index_projection.rs:440-457`:
```rust
let transcript_project = transaction.query_row(
    "SELECT project_key FROM canonical_sessions WHERE session_key = ?1", ...).optional()?;
let (transcript_status, join_conflict) = match transcript_project {
    Some(project_key) if project_key != *decisive_project_key => ("different_project", true),
    Some(_) => ("present", false),
    None => ("missing", false),
};
```
Module doc `:1-7`: *"A native index is useful metadata, but it is not transcript evidence."* Snapshot ownership guard `:82-92`. Change topics `:531`, `:551`, `:566`, `:589`.

**Catalog (third read).** `catalog_discovery.rs:250-270` depth-2 selection includes session dirs but never recurses ("a deep `subagents/` or `workflows/` tree costs nothing"). Three evidence classes: SessionDirectory/Exact, transcript file (adds size+mtime, `transcript_present`), NativeProjectIndex/NativeClaimed. `merge_index_entry` (`:275-317`) refines-but-never-overwrites; `insert_session` (`:326-364`) merges agreeing evidence, conflicts otherwise.

---

## 6. Slug ↔ cwd

No encoder exists here. One decoder — `slug_to_path` (`project_parser.rs:1153-1192`), plus one deliberate refusal.

`slug_shape` (`:1094-1131`) classifies `-Users-me-app` → (`/`,`/`), `D--Projects-app` / `D:-Projects-app` → (`D:\`,`\`); drive letter uppercased; a leading `-` always wins POSIX (`-` is not alphabetic), pinned `:1961-1965`. Then a **greedy longest-match-first filesystem probe** disambiguates hyphens (`:1159-1191`), because `-Users-me-Projects-vibe-ctl` is genuinely ambiguous; no backtracking; degrades to naive split when nothing exists (`:1133-1152`). Callers `:164`, `:578`. Tests `:1931-1984`.

The refusal — `catalog_discovery.rs:186-191`: *"The slug cannot be reversed safely because '-' may be either a path separator or part of a directory name."* Uses one bounded transcript-head read for `cwd` instead (`:212-219`, newest transcript chosen `:145-155`). Path B never decodes: `native_project_key` **is** the slug (`adapter.rs:1878`; `catalog_discovery.rs:20-22`), cwd taken from the record (`adapter.rs:1851` → `:1879`). Enumeration is a plain sorted `read_dir` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/orchestrate/ingest.rs:1043-1064`. (`platform_path_key`, `source/file.rs:465-473`, is a source-instance identity key, not a slug codec.)

---

## 7. Divergences and sharp edges

1. **`^agent-(a.+)\.jsonl$` vs `^agent-(.+)\.jsonl$` vs `strip_prefix`.** `project_parser.rs:50` requires the id to start with `a`; `fingerprint.rs:94` and `adapter.rs:1072` do not. A non-`a` id yields `agent_id = "agent-<id>"` on Path A, which then fails the substring test at `core/writer.rs:1143` and silently downgrades `link_method` to `"unlinked"`.
2. **Path A's nested walk is one level deep** (`:682-694`); the declared glob and Path B allow arbitrary depth (`adapter.rs:354`, fixture `:4058`).
3. **Path A's flat walk accepts any `*.jsonl`** (`:661`) — a stray file becomes a `SubagentTranscript` with the bare stem as `agent_id`.
4. **Subagent messages are re-serialized** (`core/writer.rs:1348`, `:1368`) — unmodelled keys lost except via `flatten extra`.
5. **`sourceToolUseID` / `sourceToolAssistantUUID` / `parentToolUseID` are dead weight** — declared, never read. Anyone expecting them to be the join key will be wrong.
6. **`spawnDepth` discarded on Path A** (`core/writer.rs:1296-1308` reads only `agent_type` + `worktree_path`).
7. **`workflow_id` unvalidated on Path A**; run id from body `runId` vs Path B rejecting mismatch (`sidecars.rs:994-1001`).
8. **`isSidechain` does no grouping anywhere** — sole effect is the `first_prompt` filter (`session_metadata.rs:60`).
The report above is complete and final. No further work is pending.

**Report delivered.** The investigation covered `project_parser.rs` in full plus `catalog_discovery.rs`, `session_index_projection.rs`, targeted `workflow_projection.rs`, and the supporting files that turned out to be load-bearing (`claude/adapter.rs`, `claude/adapter/sidecars.rs`, `claude/types/session.rs`, `claude/types/project.rs`, `claude/fingerprint.rs`, `core/writer.rs`, `claude/message_extractor.rs`, `claude/session_metadata.rs`, `claude/fts_text.rs`, `core/schema.rs`).

The single most important structural finding, which reframes all six questions: **the crate contains two parallel ingest paths** — the legacy `ProjectParser` (Path A, ported from the TS SDK) and the newer declarative adapter (Path B, RFC 012A/012C fact model). They are both live and they disagree in ways that matter, most notably the `^agent-(a.+)\.jsonl$` regex constraint that silently breaks spawn linkage for agent ids not beginning with `a`, and Path A's one-level-deep workflow walk that misses transcripts the declared `**` glob covers.
# Claude Code subagent-transcript discovery & parsing in `spaghetti-napi`

All paths relative to `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/`. Absolute paths given at first mention.

---

## 0. Orientation — there are TWO parallel ingest paths

This framing changes every answer below. Both are live in the crate.

| | **Path A — legacy stream parser** | **Path B — adapter / ADS** |
|---|---|---|
| Entry | `ProjectParser::parse_project` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/project_parser.rs:116` | `ClaudeCodeAdapter::streams` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter.rs:328` |
| Discovery | `std::fs::read_dir` walks, hand-rolled per artifact | Declarative `ObjectSelector { root_name, include: [glob] }` |
| Output | `IngestEvent` over `crossbeam_channel` → `core/writer.rs` → `subagents`, `subagent_messages`, `workflows` | `Fact::*` → `engine/*_projection.rs` → `canonical_*` |
| Warm start | `claude/fingerprint.rs` (`source_files` categories) | catalog + cursors (`claude/catalog_discovery.rs`) |
| Ported from | `packages/sdk/src/parser/project-parser.ts` (comments say so verbatim) | RFC 012A/012C native fact model |

Path A is the PRIMARY FILE. Path B holds the authoritative glob strings.

---

## 1. Path patterns, globs, ID extraction

### 1.1 Declarative globs (Path B)

```rust
// adapter.rs:347-357 — SUBAGENT_STREAM
selector: ObjectSelector {
    root_name: "projects".to_string(),
    include: vec![
        "*/*/subagents/**/agent-*.jsonl".to_string(),
        "*/*/subagents/workflows/*/**/agent-*.jsonl".to_string(),
    ],
    exclude: Vec::new(),
},
```
```rust
// adapter.rs:374 — SUBAGENT_META_STREAM
include: vec!["*/*/subagents/**/agent-*.meta.json".to_string()],
// adapter.rs:544 — WORKFLOW_JOURNAL_STREAM
include: vec!["*/*/subagents/workflows/*/journal.jsonl".to_string()],
```

| Stream | line | root | glob(s) |
|---|---|---|---|
| `PARENT_STREAM` | 335 | projects | `*/*.jsonl` |
| `SUBAGENT_STREAM` | 353-354 | projects | `*/*/subagents/**/agent-*.jsonl`, `*/*/subagents/workflows/*/**/agent-*.jsonl` |
| `SUBAGENT_META_STREAM` | 374 | projects | `*/*/subagents/**/agent-*.meta.json` |
| `WORKFLOW_RUN_STREAM` | 527 | projects | `*/*/workflows/wf_*.json` |
| `WORKFLOW_JOURNAL_STREAM` | 544 | projects | `*/*/subagents/workflows/*/journal.jsonl` |
| `SESSION_INDEX_STREAM` | 563 | projects | `*/sessions-index.json` |
| `PROJECT_MEMORY_STREAM` | 582 | projects | `*/memory/*.md` |
| `PERSISTED_TOOL_RESULT_STREAM` | 601 | projects | `*/*/tool-results/*.txt` |
| `TODO_STREAM` | 451 | home | `todos/*-agent-*.json` |
| `TASK_ITEM_STREAM` | 470 | home | `tasks/*/*.json` |
| `PLAN_STREAM` | 489 | home | `plans/*.md` |
| `ARTIFACT_CONTENT_STREAM` | 508 | home | `file-history/*/*@v*` |
| `TEAM_CONFIG_STREAM` | 393 | teams | `*/config.json` |
| `TEAM_INBOX_STREAM` | 412 | teams | `*/inboxes/*.json` |
| `ACTIVE_SESSION_STREAM` | 432 | sessions | `*.json` |
| `INTERPRETATION_SETTINGS_STREAM` | 620-623 | home | `settings.json`, `settings.local.json` |

Roots at `adapter.rs:296-313`: `home` = `~/.claude`, `projects`/`teams`/`sessions` beneath it.

### 1.2 Imperative walks (Path A) — asymmetric

```rust
// project_parser.rs:647-698
fn read_subagents(project_dir: &Path, session_id: &str)
    -> Vec<(SubagentTranscript, Option<DroppedSubagentLines>)> {
    let dir = project_dir.join(session_id).join("subagents");
    let mut out = Vec::new();

    // Top-level subagent transcripts (not associated with a workflow).
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            ...
            if !name.ends_with(".jsonl") { continue; }   // ANY *.jsonl — no agent- prefix required
            out.push(read_one_subagent(&entry.path(), name, ""));
        }
    }

    // Nested workflow subagent transcripts:
    //   subagents/workflows/{wf_id}/agent-*.jsonl  (journal.jsonl is skipped
    //   by the `agent-` prefix). ...
    if let Ok(wf_dirs) = std::fs::read_dir(dir.join("workflows")) {
        for wf_entry in wf_dirs.flatten() {
            if !wf_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
            let wf_name = wf_entry.file_name();
            let Some(workflow_id) = wf_name.to_str() else { continue; };
            if let Ok(agent_dir) = std::fs::read_dir(wf_entry.path()) {
                for entry in agent_dir.flatten() {
                    ...
                    if !name.starts_with("agent-") || !name.ends_with(".jsonl") { continue; }
                    out.push(read_one_subagent(&entry.path(), name, workflow_id));
                }
            }
        }
    }
    out
}
```

- Top-level (`:661`) accepts **any** `*.jsonl`; nested (`:688`) requires `agent-` — that is what excludes `journal.jsonl`.
- **Nested walk is one level only.** The glob and Path B allow `subagents/workflows/*/**/agent-*.jsonl` (`adapter.rs:354`; fixture `adapter.rs:4058` uses `.../wf_main/agents/agent-child.jsonl`). Path A misses those.
- `workflow_id` = raw directory name, unvalidated.

### 1.3 `agent-{id}.meta.json`

```rust
// project_parser.rs:746-749
// Sibling `agent-{id}.meta.json` carries the real agent type + description.
let meta = std::fs::read_to_string(path.with_extension("meta.json"))
    .ok()
    .and_then(|raw| serde_json::from_str::<SubagentMeta>(&raw).ok());
```
`with_extension` replaces only the final extension → `agent-a1.jsonl` → `agent-a1.meta.json`. Missing/malformed is silently `None`.

`SubagentMeta` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/project.rs:93-106`:
```rust
pub struct SubagentMeta {
    pub agent_type: String,
    // Absent on nested workflow metas — must be optional or their parse fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub name: Option<String>,
    pub spawn_depth: Option<f64>,
    pub worktree_path: Option<String>,
}
```
Path B's richer variant adds `teamName` + `toolUseId` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter/sidecars.rs:13-29`. Real fixture at `adapter.rs:7045`:
`{"agentType":"Explore","description":"map the parser","name":"survey","spawnDepth":2,"worktreePath":"/repo/.worktrees/a1","toolUseId":"tool-7"}`

### 1.4 `agentId` from filename — the divergence

```rust
// project_parser.rs:49-50
/// Matches `agent-<id>.jsonl` where `<id>` starts with `a`. Port of
/// `extractAgentId`'s `^agent-(a.+)\.jsonl$` pattern.
static SUBAGENT_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^agent-(a.+)\.jsonl$").expect("SUBAGENT_FILE regex compiles"));
```
```rust
// project_parser.rs:860-868
fn extract_agent_id(file_name: &str) -> String {
    if let Some(caps) = SUBAGENT_FILE.captures(file_name) {
        caps.get(1).map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| file_name.trim_end_matches(".jsonl").to_owned())
    } else {
        file_name.trim_end_matches(".jsonl").to_owned()
    }
}
```
The `a` is inside the capture, so `agent-abc123.jsonl` → `"abc123"` (asserted `:1435`). But an id not starting with `a` fails the regex and falls back to the **whole stem including `agent-`**. Compare `fingerprint.rs:93-94` (`^agent-(.+)\.jsonl$`) and `adapter.rs:1071-1077`:
```rust
let Some(agent_id) = file_name
    .strip_prefix("agent-")
    .and_then(|name| name.strip_suffix(suffix))
    .filter(|value| !value.is_empty())
else { return Err(path_error(relative_path, "invalid subagent object name")); };
```
`agent-b7.jsonl` → Path A `"agent-b7"`, Path B `"b7"`.

### 1.5 sessionId / workflowId from directories

Path A never parses `session_id` from a subagent path — it is passed in (`:382`). Path B — `adapter.rs:1062-1087`:
```rust
if components.len() < 4 || components.get(2).map(String::as_str) != Some("subagents") { ...err... }
...
let workflow_id = components.windows(2).find_map(|pair| {
    (pair[0] == "workflows" && !pair[1].is_empty()).then(|| pair[1].clone())
});
Ok(Self { project_slug: components[0].clone(), session_id: components[1].clone(),
          agent_id: Some(agent_id.to_string()), workflow_id })
```
Suffix dispatch `.jsonl`/`.meta.json` at `:1054-1060`. Journal path strict-shaped at `adapter.rs:1534-1552` (6 components, `[2]=="subagents"`, `[3]=="workflows"`, `[5]=="journal.jsonl"`); run doc at `:1515-1532`; `validate` requires `is_uuid(session)` + `wf_` prefix (`:1554-1580`).

### 1.6 `workflows/{runId}.json` (Path A)

```rust
// project_parser.rs:776
if !name.starts_with("wf_") || !name.ends_with(".json") { continue; }
// :786-790
let workflow_id = data.get("runId").and_then(|v| v.as_str()).map(str::to_owned)
    .unwrap_or_else(|| name.trim_end_matches(".json").to_owned());
```
Id from the **body's `runId`**, filename as fallback. Path B instead *rejects* on mismatch (`sidecars.rs:994-1001`). `subagent_count` recounted at `:802`/`:823-841`; journal read at `:843-857`; runs sorted by id (`:819`).

### 1.7 Other Path A artifacts

| Artifact | Pattern | Code |
|---|---|---|
| session JSONL | `^[0-9a-f]{8}-...-[0-9a-f]{12}\.jsonl$` | `:42-45` |
| tool results | `*.txt`, id = stem | `:888-913` |
| file history | `^([0-9a-f]+)@v(\d+)$` | `:54-56`, `:917-968` |
| todos | prefix `{sid}-agent-` then `^(.+?)-agent-(.+)\.json$` | `:60-61`, `:972-1013` |
| tasks | `.lock` / `.highwatermark`, `parseInt`-lenient | `:1017-1040` |
| plans | `plans/*.md`, title `(?m)^#\s+(.+)$` | `:1222-1275` |
| memory | `memory/MEMORY.md` | `:640-643` |

Warm-start taxonomy `fingerprint.rs:30-40`; `CATEGORY_SUBAGENT_META` / `CATEGORY_WORKFLOW_JOURNAL` at `:79-82` exist because "a changed agent type, a changed journal, or a whole workflow's transcripts were invisible to warm start — RFC 008 Phase 1.4" (`:608-616`).

---

## 2. Subagent vs main transcript parsing

**Same `SessionMessage` enum — nothing else is shared.**

Main line: `project_parser.rs:309-356` → `build_message_event` (`:458-485`) → `message_extractor::project_jsonl_line` (`message_extractor.rs:50-57`): loose `Value` parse is authoritative for `msg_type`/`uuid`/`timestamp`; typed parse is only for FTS; **raw line stored verbatim** as `raw_json` (`:474`); a typed failure keeps the row and merely reports lost FTS.

Subagent line — `project_parser.rs:715-761`:
```rust
let _ = read_jsonl_streaming(path, 0, |line, idx, _off| {
    match serde_json::from_str::<SessionMessage>(line) {
        Ok(msg) => messages.push(msg),
        Err(e) => {
            let msg_type = serde_json::from_str::<Value>(line).ok()
                .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            if !fts_text::type_contributes_text(&msg_type) { return; }
            ...
        }
    }
});
```

Differences:
1. **Typed parse is the only parse and is load-bearing** — a failure means the line is gone. Doc `:700-708`: *"the transcript is the only thing holding it, so there is no verbatim JSON to fall back on."*
2. **Re-serialized, not verbatim** — `core/writer.rs:1347-1371`: `let value = serde_json::to_value(message)?;` … `serde_json::to_string(&value)?`. Unmodelled keys are lost except via `#[serde(flatten)] extra` (`session.rs:400-403`, `:213-214`). Timestamp re-read from the re-encoded value.
3. Separate token extractor `core/writer.rs:1151-1171`.
4. **No FTS column** on `subagent_messages` (`core/schema.rs:358-373`).
5. Drops reported only for content-bearing types — `fts_text.rs:157-162`: `matches!(msg_type, "user"|"assistant"|"system"|"summary"|"ai-title")`.
6. **Read errors swallowed** (`let _ =`, `:722`) vs checked on the main path (`:362-379`).
7. `message_count` = successfully-typed lines (`core/writer.rs:1302`).
8. Empty transcripts → `subagent_dirty_threads` (`core/writer.rs:1372-1387`).

Enum at `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/session.rs:23-52` (`#[serde(tag="type", rename_all="kebab-case")]`, 17 variants + `#[serde(other)] Unknown`). Two hardenings exist *because* of subagents: `AssistantMessage.request_id` defaulted (`session.rs:355-359`, test `:1851-1888`) and `image_paste_ids` widened to `Option<Value>` (`session.rs:291-300`) — both previously "dropped it from subagent transcripts."

Path B makes no distinction: `adapter.rs:729-754` routes both decoders into the same `decode_transcript_record`, guarding only on `agent_id` presence; raw bytes kept (`:2032`).

---

## 3. Sidechain and parent linkage

### `isSidechain` does no grouping
Modelled at `session.rs:75-76` and `project.rs:55-56`. Its **only** behavioural use — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/session_metadata.rs:57-75`:
```rust
if record.get("type").and_then(Value::as_str) != Some("user")
    || record.get("isMeta").and_then(Value::as_bool) == Some(true)
    || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
    || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
    || record.get("isVisibleInTranscriptOnly").and_then(Value::as_bool) == Some(true)
{ return None; }
```
Elsewhere it is stored/projected only: `core/schema.rs:279`/`:878`/`:913`, `core/writer.rs:1215`, `session_index_projection.rs:249`/`:471-474`, read back in `history_query.rs:486`, `detail_query.rs:494`, `query_pool.rs:260`.

### Path A linkage — substring match on `tool_result`
`core/writer.rs:1090-1149`:
```rust
/// Port of the TS `resolveSubagentSpawnToolId`. Claude records a subagent's
/// result as a `tool_result` block in the parent session whose content
/// mentions the agent id; that block's `tool_use_id` is the spawning call.
/// Without this ... silently dropped the linkage for 113 of 638 subagents (RFC 008 Phase 5).
... "SELECT data FROM messages WHERE session_id = ?1 AND source_id = ?2 AND data LIKE '%' || ?3 || '%' ORDER BY msg_index"
    if body.contains(agent_id) { return Ok(Some(tool_use_id.to_owned())); }
```
Consumed `:1309-1320` → `spawn_tool_id`, `link_method ∈ {"tool_result","unlinked"}`.

### Path B linkage — layout + `toolUseId`
`run_native_key` = `sessionId \0 workflowId \0 agentId` (`adapter.rs:1093-1103`); parent run = bare sessionId when `agent_id.is_some()` (`:1890-1899`); role `Child`/`Root` (`:1925-1929`); `DelegationFact { kind: VendorNativeSubagent, relation_strength: RelationStrength::Layout, native_task_id: None }` (`:1938-1959`). Spawn side from `Task`/`Agent` tool_use blocks (`:2488-2514`, reading `description`/`name`, `prompt`, `subagent_type`/`agent_type`). The join closes via the sidecar: `native_task_id: document.tool_use_id...` (`adapter.rs:1774`).

### Field verdict
| Field | Modelled | Used? |
|---|---|---|
| `agentId` (filename) | `project_parser.rs:860-868`; `adapter.rs:1071-1077` | **Primary child identity** both paths |
| `agentId` (in-record) | `session.rs:277-278`, `:361-362`, `:431-432`, `:478-479` | **Never read** |
| `sourceToolUseID` | `session.rs:271-276` | **Never read** |
| `sourceToolAssistantUUID` | `session.rs:265-270` | **Never read** |
| `parentToolUseID` | `session.rs:429-430` | **Never read** |
| `parentUuid` | `session.rs:65-66`; `adapter.rs:1983` | Path B → `MessageFact.parent_native_message_id` (`:2029`) — intra-transcript only |
| `workflowId` | `project_parser.rs:678-680`; `adapter.rs:1078-1080` | Uniqueness key (`schema.rs:355`); middle key segment |
| `spawnDepth` | `project.rs:103`; `sidecars.rs:24`; `facts.rs:2063` | Path A **discarded**; Path B projected (`schema.rs:1219`, `projection.rs:2727`, `orchestration_query.rs:1019`) |
| `isSidechain` | above | first_prompt filter only |

---

## 4. `SubagentType` + nested/workflow differences

```rust
// types/project.rs:63-71
/// TS `SubagentType = 'task' | 'prompt_suggestion' | 'compact'`.
#[serde(rename_all = "snake_case")]
pub enum SubagentType { Task, PromptSuggestion, Compact }
```
```rust
// project_parser.rs:876-884
fn infer_agent_type(file_name: &str) -> SubagentType {
    if file_name.contains("prompt_suggestion") { SubagentType::PromptSuggestion }
    else if file_name.contains("compact") { SubagentType::Compact }
    else { SubagentType::Task }
}
```
Order-dependent, `contains` (not prefix/suffix), run over the whole filename. **It is only a fallback** — `core/writer.rs:1292-1301`: the sidecar's free-form `agentType` (`"Explore"`, `"general-purpose"`) wins; the enum is serialized and de-quoted only when no sidecar parses. Path B has no such enum (`adapter.rs:1736-1744` requires a non-empty `agentType` or preserves unknown).

| Aspect | Flat | Workflow-nested |
|---|---|---|
| Path A filter | any `*.jsonl` (`:661`) | `agent-` required (`:688`) |
| Depth | flat | one level only |
| `workflow_id` | `""` (`:664`; `project.rs:86-89`) | dir name (`:691`) |
| `run_native_key` | `sid \0 "" \0 agentId` | `sid \0 wf_x \0 agentId` |
| Sidecar `description` | present | **absent** (`project.rs:97`) |
| Extra evidence | none | `journal.jsonl` → `WorkflowMemberEvent` (`sidecars.rs:1069-1219`) |
| wf id validation | n/a | Path A none; Path B `wf_`+suffix (`adapter.rs:1566-1574`) |

Journal kinds strictly `started` (no `result`) / `result` (`sidecars.rs:1116-1145`) → `member_status ∈ {result_observed, started, orphan_result}` (`workflow_projection.rs:472-481`). Count reconciliation `workflow_projection.rs:634-641` → `{unobserved, snapshot_missing, matched, different}`; join check `:607-621`. Terminal workflow state never propagated to children (`:5-7`).

---

## 5. sessions-index.json read + reconciliation

**Path A.** Read (`:489-509`) — missing *and* malformed both degrade to an empty synthetic index. Merge (`:511-535`) — index entries win, discovered entries appended **sorted** ("NTFS returns entries sorted while ext4 and APFS do not… RFC 008 Phase 5"). Discovery (`:545-636`) — `read_dir` failure is **project-fatal** (`:558-573`), `NotFound` is not; mtime computed Node-style (`:596-606`); `first_prompt` peeked (`:616`, `:1199-1215`, 200 UTF-16 units); `summary: Some("")` deliberately (`project.rs:33-42`, "6 NULL and 117 empty"). The **merged** index is persisted (`:178-189`), test `:1735-1777`. Per-session file resolution falls back to `entry.full_path` (`:290-299`); an index entry with no file still emits `Session` + `SessionComplete` with count 0.

**Path B.** `Supplemental` authority (`adapter.rs:567`), strict decode (`sidecars.rs:1221-1343`) — any of version≠1, >4096 entries, non-UUID/duplicate `sessionId`, blank required field preserves the whole doc as contract loss. Reconciliation in `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/engine/session_index_projection.rs:440-457`:
```rust
let transcript_project = transaction.query_row(
    "SELECT project_key FROM canonical_sessions WHERE session_key = ?1", ...).optional()?;
let (transcript_status, join_conflict) = match transcript_project {
    Some(project_key) if project_key != *decisive_project_key => ("different_project", true),
    Some(_) => ("present", false),
    None => ("missing", false),
};
```
Module doc `:1-7`: *"A native index is useful metadata, but it is not transcript evidence."* Snapshot ownership guard `:82-92`. Change topics `:531`, `:551`, `:566`, `:589`.

**Catalog (third read).** `catalog_discovery.rs:250-270` depth-2 selection includes session dirs but never recurses ("a deep `subagents/` or `workflows/` tree costs nothing"). Three evidence classes: SessionDirectory/Exact, transcript file (adds size+mtime, `transcript_present`), NativeProjectIndex/NativeClaimed. `merge_index_entry` (`:275-317`) refines-but-never-overwrites; `insert_session` (`:326-364`) merges agreeing evidence, conflicts otherwise.

---

## 6. Slug ↔ cwd

No encoder exists here. One decoder — `slug_to_path` (`project_parser.rs:1153-1192`), plus one deliberate refusal.

`slug_shape` (`:1094-1131`) classifies `-Users-me-app` → (`/`,`/`), `D--Projects-app` / `D:-Projects-app` → (`D:\`,`\`); drive letter uppercased; a leading `-` always wins POSIX (`-` is not alphabetic), pinned `:1961-1965`. Then a **greedy longest-match-first filesystem probe** disambiguates hyphens (`:1159-1191`), because `-Users-me-Projects-vibe-ctl` is genuinely ambiguous; no backtracking; degrades to naive split when nothing exists (`:1133-1152`). Callers `:164`, `:578`. Tests `:1931-1984`.

The refusal — `catalog_discovery.rs:186-191`: *"The slug cannot be reversed safely because '-' may be either a path separator or part of a directory name."* Uses one bounded transcript-head read for `cwd` instead (`:212-219`, newest transcript chosen `:145-155`). Path B never decodes: `native_project_key` **is** the slug (`adapter.rs:1878`; `catalog_discovery.rs:20-22`), cwd taken from the record (`adapter.rs:1851` → `:1879`). Enumeration is a plain sorted `read_dir` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/orchestrate/ingest.rs:1043-1064`. (`platform_path_key`, `source/file.rs:465-473`, is a source-instance identity key, not a slug codec.)

---

## 7. Divergences and sharp edges

1. **`^agent-(a.+)\.jsonl$` vs `^agent-(.+)\.jsonl$` vs `strip_prefix`.** `project_parser.rs:50` requires the id to start with `a`; `fingerprint.rs:94` and `adapter.rs:1072` do not. A non-`a` id yields `agent_id = "agent-<id>"` on Path A, which then fails the substring test at `core/writer.rs:1143` and silently downgrades `link_method` to `"unlinked"`.
2. **Path A's nested walk is one level deep** (`:682-694`); the declared glob and Path B allow arbitrary depth (`adapter.rs:354`, fixture `:4058`).
3. **Path A's flat walk accepts any `*.jsonl`** (`:661`) — a stray file becomes a `SubagentTranscript` with the bare stem as `agent_id`.
4. **Subagent messages are re-serialized** (`core/writer.rs:1348`, `:1368`) — unmodelled keys lost except via `flatten extra`.
5. **`sourceToolUseID` / `sourceToolAssistantUUID` / `parentToolUseID` are dead weight** — declared, never read. Anyone expecting them to be the join key will be wrong.
6. **`spawnDepth` discarded on Path A** (`core/writer.rs:1296-1308` reads only `agent_type` + `worktree_path`).
7. **`workflow_id` unvalidated on Path A**; run id from body `runId` vs Path B rejecting mismatch (`sidecars.rs:994-1001`).
8. **`isSidechain` does no grouping anywhere** — sole effect is the `first_prompt` filter (`session_metadata.rs:60`).
The report is complete and was delivered in full in my previous message. The investigation is finished — no work remains.

**Files examined:** `project_parser.rs` (all 2023 lines, two reads due to the token cap), `catalog_discovery.rs` (437), `session_index_projection.rs` (729), targeted `workflow_projection.rs`, plus the supporting files that proved load-bearing: `claude/adapter.rs`, `claude/adapter/sidecars.rs`, `claude/types/session.rs`, `claude/types/project.rs`, `claude/types/workflow.rs`, `claude/fingerprint.rs`, `core/writer.rs`, `core/schema.rs`, `claude/message_extractor.rs`, `claude/session_metadata.rs`, `claude/fts_text.rs`, `orchestrate/ingest.rs`.

**The one structural fact that reframes all six questions:** the crate carries two parallel ingest paths — the legacy `ProjectParser` (`project_parser.rs:116`, a direct port of `packages/sdk/src/parser/project-parser.ts`) and the newer declarative adapter (`adapter.rs:328`, RFC 012A/012C fact model). Both are live, and they disagree on agent-id extraction, walk depth, filename filtering, and workflow-id validation.

**Two divergences are worth acting on:**
- `^agent-(a.+)\.jsonl$` (`project_parser.rs:50`) requires agent ids to begin with `a`; `fingerprint.rs:94` and `adapter.rs:1072` do not. A non-`a` id makes Path A store `agent_id = "agent-<id>"`, which then fails the substring probe at `core/writer.rs:1143` and silently degrades `link_method` to `"unlinked"`.
- Path A's nested workflow walk is one level deep (`project_parser.rs:682-694`) while the declared glob and Path B allow arbitrary depth (`adapter.rs:354`, fixture at `:4058`).

**Answers in brief:** globs are in `adapter.rs` (`:353-354`, `:374`, `:544`, `:527`, `:563`); subagent transcripts are typed-parse-only with no `raw_json` fallback and are re-serialized through serde before storage; `isSidechain` performs no grouping anywhere and `sourceToolUseID`/`sourceToolAssistantUUID`/`parentToolUseID` are declared but never read — linkage is filename-`agentId` + `tool_result` substring (Path A) or directory layout + sidecar `toolUseId` (Path B); `SubagentType` is a filename `contains` test that only acts as a fallback behind the sidecar's `agentType`; `sessions-index.json` is merged with disk discovery on Path A and treated as `Supplemental` evidence joined to `canonical_sessions` on Path B; and slug decoding uses a greedy filesystem probe (`project_parser.rs:1153-1192`) that the catalog path deliberately declines to use in favour of reading `cwd` from the newest transcript head.
# Claude Code subagent-transcript discovery & parsing in `spaghetti-napi`

All paths relative to `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/`. Absolute paths given at first mention.

---

## 0. Orientation — there are TWO parallel ingest paths in this crate

This framing changes every answer below. Both are live and compiled.

| | **Path A — legacy stream parser** | **Path B — adapter / ADS** |
|---|---|---|
| Entry | `ProjectParser::parse_project` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/project_parser.rs:116` | `ClaudeCodeAdapter::streams` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter.rs:328` |
| Discovery | `std::fs::read_dir` walks, hand-rolled per artifact type | Declarative `ObjectSelector { root_name, include: [glob] }` |
| Output | `IngestEvent` over a `crossbeam_channel` → `core/writer.rs` → tables `subagents`, `subagent_messages`, `workflows` | `Fact::*` → `engine/*_projection.rs` → `canonical_*` tables |
| Warm start | `claude/fingerprint.rs` (`source_files` categories) | catalog + cursors (`claude/catalog_discovery.rs`) |
| Ported from | `packages/sdk/src/parser/project-parser.ts` (module doc says so verbatim) | RFC 012A/012C native fact model |

Path A is the PRIMARY FILE. Path B holds the authoritative glob strings.

Path A's module header, `project_parser.rs:1-16`:

```rust
//! Per-project streaming parser — ported from
//! `packages/sdk/src/parser/project-parser.ts`.
//!
//! Single-threaded: given one `<root_dir>/projects/<slug>/` directory,
//! walks every artifact (sessions index, MEMORY.md, JSONL session files,
//! subagent transcripts, tool-result .txt files, todos, tasks, file-history
//! snapshots) and pushes one [`IngestEvent`] per discovered artifact into a
//! [`crossbeam_channel::Sender`].
//!
//! Parse errors inside an individual session or file are swallowed and
//! re-emitted as [`IngestEvent::RecordSkip`] — this matches the TS
//! parser's behaviour of wrapping each sub-parse in its own `try/catch`.
//! The only error the caller sees is a channel-send failure, which is
//! fatal (the writer has gone away).
```

---

## 1. Path patterns, globs, and ID extraction

### 1.1 The declarative globs (Path B — `claude/adapter.rs`)

These are the literal strings. Root `"projects"` = `<claude_home>/projects`.

```rust
// adapter.rs:347-366  — SUBAGENT_STREAM
StreamSpec {
    id: StreamId::new(SUBAGENT_STREAM)?,
    driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
    selector: ObjectSelector {
        root_name: "projects".to_string(),
        include: vec![
            "*/*/subagents/**/agent-*.jsonl".to_string(),
            "*/*/subagents/workflows/*/**/agent-*.jsonl".to_string(),
        ],
        exclude: Vec::new(),
    },
    decoder: DecoderId::new(SUBAGENT_DECODER)?,
    authority: StreamAuthority::Canonical,
    entity_scope: EntityScope::Run,
    priority: IngestPriority::Interactive,
    consistency: ConsistencyPolicy::IncrementalCursor,
    deletion: DeletionPolicy::MirrorSource,
    retention: RawRetentionPolicy::HashOnly,
    capabilities: transcript_capabilities(),
},
```

```rust
// adapter.rs:367-385 — SUBAGENT_META_STREAM
StreamSpec {
    id: StreamId::new(SUBAGENT_META_STREAM)?,
    driver: DriverSpec::ReplaceDocument(ReplaceDocumentConfig {
        max_document_bytes: SUBAGENT_META_MAX_BYTES,
    }),
    selector: ObjectSelector {
        root_name: "projects".to_string(),
        include: vec!["*/*/subagents/**/agent-*.meta.json".to_string()],
        exclude: Vec::new(),
    },
    decoder: DecoderId::new(SUBAGENT_META_DECODER)?,
    authority: StreamAuthority::Supplemental,
    entity_scope: EntityScope::Run,
    priority: IngestPriority::ForegroundRepair,
    consistency: ConsistencyPolicy::SnapshotReplace,
    deletion: DeletionPolicy::MirrorSource,
    retention: RawRetentionPolicy::HashOnly,
    capabilities: subagent_metadata_capabilities(),
},
```

```rust
// adapter.rs:520-538 — WORKFLOW_RUN_STREAM
selector: ObjectSelector {
    root_name: "projects".to_string(),
    include: vec!["*/*/workflows/wf_*.json".to_string()],
    exclude: Vec::new(),
},
```

```rust
// adapter.rs:539-555 — WORKFLOW_JOURNAL_STREAM
StreamSpec {
    id: StreamId::new(WORKFLOW_JOURNAL_STREAM)?,
    driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
    selector: ObjectSelector {
        root_name: "projects".to_string(),
        include: vec!["*/*/subagents/workflows/*/journal.jsonl".to_string()],
        exclude: Vec::new(),
    },
    decoder: DecoderId::new(WORKFLOW_JOURNAL_DECODER)?,
    authority: StreamAuthority::Canonical,
    entity_scope: EntityScope::Custom("workflow_member".to_string()),
    priority: IngestPriority::Interactive,
    consistency: ConsistencyPolicy::IncrementalCursor,
    deletion: DeletionPolicy::MirrorSource,
    retention: RawRetentionPolicy::HashOnly,
    capabilities: workflow_capabilities(),
},
```

```rust
// adapter.rs:556-574 — SESSION_INDEX_STREAM
selector: ObjectSelector {
    root_name: "projects".to_string(),
    include: vec!["*/sessions-index.json".to_string()],
    exclude: Vec::new(),
},
...
authority: StreamAuthority::Supplemental,
entity_scope: EntityScope::Project,
```

Complete glob table (`adapter.rs`):

| Stream const | line | root | include glob(s) |
|---|---|---|---|
| `PARENT_STREAM` (`session-transcripts`) | 335 | projects | `*/*.jsonl` |
| `SUBAGENT_STREAM` | 353-354 | projects | `*/*/subagents/**/agent-*.jsonl`, `*/*/subagents/workflows/*/**/agent-*.jsonl` |
| `SUBAGENT_META_STREAM` | 374 | projects | `*/*/subagents/**/agent-*.meta.json` |
| `WORKFLOW_RUN_STREAM` | 527 | projects | `*/*/workflows/wf_*.json` |
| `WORKFLOW_JOURNAL_STREAM` | 544 | projects | `*/*/subagents/workflows/*/journal.jsonl` |
| `SESSION_INDEX_STREAM` | 563 | projects | `*/sessions-index.json` |
| `PROJECT_MEMORY_STREAM` | 582 | projects | `*/memory/*.md` |
| `PERSISTED_TOOL_RESULT_STREAM` | 601 | projects | `*/*/tool-results/*.txt` |
| `TODO_STREAM` | 451 | home | `todos/*-agent-*.json` |
| `TASK_ITEM_STREAM` | 470 | home | `tasks/*/*.json` |
| `PLAN_STREAM` | 489 | home | `plans/*.md` |
| `ARTIFACT_CONTENT_STREAM` | 508 | home | `file-history/*/*@v*` |
| `TEAM_CONFIG_STREAM` | 393 | teams | `*/config.json` |
| `TEAM_INBOX_STREAM` | 412 | teams | `*/inboxes/*.json` |
| `ACTIVE_SESSION_STREAM` | 432 | sessions | `*.json` |
| `INTERPRETATION_SETTINGS_STREAM` | 620-623 | home | `settings.json`, `settings.local.json` |

Roots are declared in `discover` at `adapter.rs:296-313`:

```rust
roots: vec![
    SourceRoot { name: "home".to_string(),     path: canonical.clone() },
    SourceRoot { name: "projects".to_string(), path: canonical.join("projects") },
    SourceRoot { name: "teams".to_string(),    path: canonical.join("teams") },
    SourceRoot { name: "sessions".to_string(), path: canonical.join("sessions") },
],
```

Streams are filtered to authorized roots and validated — `adapter.rs:636-646`:

```rust
let authorized_roots = instance.spec.roots.iter()
    .map(|root| root.name.as_str())
    .collect::<BTreeSet<_>>();
streams.retain(|stream| authorized_roots.contains(stream.selector.root_name.as_str()));
for stream in &streams { stream.validate(instance)?; }
```

There is also a documented locator/source_pattern manifest asserted in tests — `adapter.rs:3845`, `:3848`, `:3857`, `:3864`, `:3867`, `:3936`, `:3939`:

```
locator: "{project-key}/{native-session-id}/subagents",
source_pattern: "*/*/subagents/**/agent-*.jsonl",
source_pattern: "*/*/subagents/**/agent-*.meta.json",
locator: "{project-key}/{native-session-id}/subagents/workflows/{observed-workflow-id}",
source_pattern: "*/*/subagents/workflows/*/**/agent-*.jsonl",
locator: "{project-key}/{native-session-id}/subagents/workflows/{observed-workflow-id}/journal.jsonl",
source_pattern: "*/*/subagents/workflows/*/journal.jsonl",
```

### 1.2 The imperative walks (Path A — `project_parser.rs`)

`read_subagents` does **two** distinct walks and they are not symmetric — `project_parser.rs:647-698`, quoted in full:

```rust
fn read_subagents(
    project_dir: &Path,
    session_id: &str,
) -> Vec<(SubagentTranscript, Option<DroppedSubagentLines>)> {
    let dir = project_dir.join(session_id).join("subagents");
    let mut out = Vec::new();

    // Top-level subagent transcripts (not associated with a workflow).
    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.ends_with(".jsonl") {
                continue;
            }
            out.push(read_one_subagent(&entry.path(), name, ""));
        }
    }

    // Nested workflow subagent transcripts:
    //   subagents/workflows/{wf_id}/agent-*.jsonl  (journal.jsonl is skipped
    //   by the `agent-` prefix). Prior to this the parser only walked the
    //   flat subagents/ dir, so workflow-orchestrated transcripts were
    //   invisible. Grouped to their run via `workflow_id`.
    if let Ok(wf_dirs) = std::fs::read_dir(dir.join("workflows")) {
        for wf_entry in wf_dirs.flatten() {
            if !wf_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let wf_name = wf_entry.file_name();
            let Some(workflow_id) = wf_name.to_str() else {
                continue;
            };
            if let Ok(agent_dir) = std::fs::read_dir(wf_entry.path()) {
                for entry in agent_dir.flatten() {
                    let file_name = entry.file_name();
                    let Some(name) = file_name.to_str() else {
                        continue;
                    };
                    if !name.starts_with("agent-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    out.push(read_one_subagent(&entry.path(), name, workflow_id));
                }
            }
        }
    }

    out
}
```

Notable asymmetries:

- **Top-level** (`project_parser.rs:661`): accepts **any** `*.jsonl` in `subagents/`, not just `agent-*`.
- **Nested** (`project_parser.rs:688`): requires the `agent-` prefix — that is precisely what excludes `journal.jsonl`.
- **Nested walk is only one level deep** — `subagents/workflows/<wf>/agent-*.jsonl` only. The adapter glob `*/*/subagents/workflows/*/**/agent-*.jsonl` allows deeper nesting (e.g. `.../wf_main/agents/agent-child.jsonl`, exercised at `adapter.rs:4058` and `adapter.rs:5852`). **Path A would miss those; Path B catches them.** Same for the `**` in the flat glob.
- `workflow_id` is literally the directory name under `workflows/` — no validation in Path A (Path B requires a `wf_` prefix, see §4).

Call site — `project_parser.rs:381-400`:

```rust
// Subagents (incl. nested workflow transcripts, tagged by workflow_id)
for (transcript, dropped) in read_subagents(project_dir, &session_id) {
    events.send(IngestEvent::Subagent {
        slug: slug.to_owned(),
        session_id: session_id.clone(),
        transcript,
    })?;
    // Lines the transcript could not type-parse are simply absent from it.
    // Report so the count is not quietly wrong and the file re-reads.
    if let Some(d) = dropped {
        events.send(IngestEvent::RecordSkip {
            slug: slug.to_owned(),
            path: d.path,
            message: format!(
                "subagent transcript dropped {} unparseable line(s); first was {}",
                d.count, d.first_error
            ),
        })?;
    }
}
```

### 1.3 `agent-{id}.meta.json` sidecar

Path A reads it by suffix substitution, not by glob — `project_parser.rs:746-749`:

```rust
// Sibling `agent-{id}.meta.json` carries the real agent type + description.
let meta = std::fs::read_to_string(path.with_extension("meta.json"))
    .ok()
    .and_then(|raw| serde_json::from_str::<SubagentMeta>(&raw).ok());
```

`Path::with_extension` replaces only the final extension, so `agent-a1.jsonl` → `agent-a1.meta.json`. A malformed or missing sidecar is silently `None` — no `RecordSkip`, no diagnostic.

`SubagentMeta` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/project.rs:92-106`:

```rust
/// `agent-{id}.meta.json` — TS `SubagentMeta`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentMeta {
    pub agent_type: String,
    // Absent on nested workflow metas — must be optional or their parse fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_depth: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
}
```

Path B's richer version adds `teamName` and `toolUseId` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/adapter/sidecars.rs:13-29`:

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeSubagentMetadataDocument {
    pub(super) agent_type: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) team_name: Option<String>,
    #[serde(default)]
    pub(super) spawn_depth: Option<u32>,
    #[serde(default)]
    pub(super) worktree_path: Option<String>,
    #[serde(default)]
    pub(super) tool_use_id: Option<String>,
}
```

A real fixture is at `adapter.rs:7045`:

```
{"agentType":"Explore","description":"map the parser","name":"survey","spawnDepth":2,"worktreePath":"/repo/.worktrees/a1","toolUseId":"tool-7"}
```

with `assert_eq!(metadata.spawn_depth, Some(2));` at `adapter.rs:7082`.

### 1.4 `agentId` extraction from the filename — and the regex divergence

**Path A** — `project_parser.rs:47-50`:

```rust
/// Matches `agent-<id>.jsonl` where `<id>` starts with `a`. Port of
/// `extractAgentId`'s `^agent-(a.+)\.jsonl$` pattern.
static SUBAGENT_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^agent-(a.+)\.jsonl$").expect("SUBAGENT_FILE regex compiles"));
```

and `project_parser.rs:859-868`:

```rust
/// TS: `const match = fileName.match(/^agent-(a.+)\.jsonl$/);`
fn extract_agent_id(file_name: &str) -> String {
    if let Some(caps) = SUBAGENT_FILE.captures(file_name) {
        caps.get(1)
            .map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| file_name.trim_end_matches(".jsonl").to_owned())
    } else {
        file_name.trim_end_matches(".jsonl").to_owned()
    }
}
```

The `a` inside `(a.+)` is **part of the capture group**, so `agent-abc123.jsonl` → `"abc123"`. The test spells this out — `project_parser.rs:1433-1435`:

```rust
// TS regex `^agent-(a.+)\.jsonl$` requires the id to start with `a`
// and captures it verbatim — so the capture here is "abc123".
assert_eq!(transcript.agent_id, "abc123");
```

But the id **must start with `a`** or the regex fails entirely and the fallback yields the **whole stem including the `agent-` prefix**: `agent-xyz.jsonl` → `"agent-xyz"`.

**This is a real cross-path divergence.** Compare the other two implementations:

`claude/fingerprint.rs:92-100` — no `a` constraint, and a separate meta pattern:

```rust
/// Matches `agent-<id>.jsonl`. Port of `extractAgentId`'s pattern.
static SUBAGENT_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^agent-(.+)\.jsonl$").expect("SUBAGENT_FILE regex compiles"));

/// Matches the `agent-<id>.meta.json` sidecar the parser reads for agent type
/// and description.
static SUBAGENT_META_FILE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^agent-(.+)\.meta\.json$").expect("SUBAGENT_META_FILE regex compiles")
});
```

`adapter.rs:1071-1077` — pure strip, no `a` constraint, and an error rather than a fallback:

```rust
let Some(agent_id) = file_name
    .strip_prefix("agent-")
    .and_then(|name| name.strip_suffix(suffix))
    .filter(|value| !value.is_empty())
else {
    return Err(path_error(relative_path, "invalid subagent object name"));
};
```

So for a hypothetical `agent-b7.jsonl`, Path A stores `agent_id = "agent-b7"` and Path B stores `"b7"`. Consequence: the Path A value then fails the substring test in `resolve_spawn_tool_id` (§3.2, `core/writer.rs:1143`) and the subagent is silently recorded with `link_method = "unlinked"`.

Note also `trim_end_matches(".jsonl")` is *repeated*-suffix trimming in Rust — `"x.jsonl.jsonl"` → `"x"`. Minor, but not identical to a single `strip_suffix`.

### 1.5 `sessionId` / `workflowId` extraction from directories

**Path A**: `session_id` is never parsed out of a subagent path — it is passed down from the enclosing session loop (`project_parser.rs:382`: `read_subagents(project_dir, &session_id)`). `workflow_id` is the raw directory name (`project_parser.rs:678-680`).

**Path B** — `adapter.rs:1022-1104`, the whole context type:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaudeTranscriptContext {
    project_slug: String,
    session_id: String,
    agent_id: Option<String>,
    workflow_id: Option<String>,
}

impl ClaudeTranscriptContext {
    fn parent(relative_path: &Path) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() != 2 {
            return Err(path_error(
                relative_path,
                "parent transcript path must have two components",
            ));
        }
        let session_id = jsonl_stem(&components[1], relative_path)?;
        if !is_uuid(&session_id) {
            return Err(path_error(
                relative_path,
                "parent transcript name is not a UUID",
            ));
        }
        Ok(Self {
            project_slug: components[0].clone(),
            session_id,
            agent_id: None,
            workflow_id: None,
        })
    }

    fn subagent(relative_path: &Path) -> Result<Self, AdapterError> {
        Self::subagent_object(relative_path, ".jsonl")
    }

    fn subagent_meta(relative_path: &Path) -> Result<Self, AdapterError> {
        Self::subagent_object(relative_path, ".meta.json")
    }

    fn subagent_object(relative_path: &Path, suffix: &str) -> Result<Self, AdapterError> {
        let components = utf8_components(relative_path)?;
        if components.len() < 4 || components.get(2).map(String::as_str) != Some("subagents") {
            return Err(path_error(
                relative_path,
                "subagent object path does not match Claude layout",
            ));
        }
        let file_name = components.last().expect("minimum component count checked");
        let Some(agent_id) = file_name
            .strip_prefix("agent-")
            .and_then(|name| name.strip_suffix(suffix))
            .filter(|value| !value.is_empty())
        else {
            return Err(path_error(relative_path, "invalid subagent object name"));
        };
        let workflow_id = components.windows(2).find_map(|pair| {
            (pair[0] == "workflows" && !pair[1].is_empty()).then(|| pair[1].clone())
        });
        Ok(Self {
            project_slug: components[0].clone(),
            session_id: components[1].clone(),
            agent_id: Some(agent_id.to_string()),
            workflow_id,
        })
    }

    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        decode_object_context(context)
    }

    fn run_native_key(&self) -> String {
        match &self.agent_id {
            Some(agent_id) => format!(
                "{}\0{}\0{}",
                self.session_id,
                self.workflow_id.as_deref().unwrap_or(""),
                agent_id
            ),
            None => self.session_id.clone(),
        }
    }
}
```

So `components[0] = project_slug`, `components[1] = session_id`, `components[2]` must literally be `"subagents"`, and `workflow_id` is the component immediately following any component named `workflows` — found by a sliding window, so it works at **any** depth.

The suffix dispatch (`.jsonl` vs `.meta.json`) is what lets one function serve both the transcript and the sidecar (`adapter.rs:1054-1060`), and it is bootstrapped per-stream at `adapter.rs:658-663`:

```rust
SUBAGENT_STREAM => {
    encode_object_context(&ClaudeTranscriptContext::subagent(&object.relative_path)?)?
}
SUBAGENT_META_STREAM => encode_object_context(&ClaudeSubagentMetadataContext {
    child: ClaudeTranscriptContext::subagent_meta(&object.relative_path)?,
})?,
```

Workflow journal path parsing is a strict fixed shape — `adapter.rs:1534-1552`:

```rust
fn journal_from_path(relative_path: &Path) -> Result<Self, AdapterError> {
    let components = utf8_components(relative_path)?;
    if components.len() != 6
        || components[2] != "subagents"
        || components[3] != "workflows"
        || components[5] != "journal.jsonl"
    {
        return Err(path_error(
            relative_path,
            "workflow journal path must be <project>/<session>/subagents/workflows/<workflow>/journal.jsonl",
        ));
    }
    Self::validate(
        relative_path,
        &components[0],
        &components[1],
        &components[4],
    )
}
```

Workflow run document path — `adapter.rs:1515-1532`:

```rust
fn run_from_path(relative_path: &Path) -> Result<Self, AdapterError> {
    let components = utf8_components(relative_path)?;
    if components.len() != 4 || components[2] != "workflows" {
        return Err(path_error(
            relative_path,
            "workflow run path must be <project>/<session>/workflows/<workflow>.json",
        ));
    }
    let native_workflow_id = components[3]
        .strip_suffix(".json")
        .ok_or_else(|| path_error(relative_path, "workflow run must be a JSON document"))?;
    Self::validate(
        relative_path,
        &components[0],
        &components[1],
        native_workflow_id,
    )
}
```

Shared validation — `adapter.rs:1554-1580`:

```rust
fn validate(
    path: &Path,
    project_slug: &str,
    native_session_id: &str,
    native_workflow_id: &str,
) -> Result<Self, AdapterError> {
    if project_slug.is_empty() {
        return Err(path_error(path, "workflow project slug must not be empty"));
    }
    if !is_uuid(native_session_id) {
        return Err(path_error(path, "workflow session is not a UUID"));
    }
    if !native_workflow_id
        .strip_prefix("wf_")
        .is_some_and(|suffix| !suffix.is_empty())
    {
        return Err(path_error(
            path,
            "workflow identity must start with wf_ and include a suffix",
        ));
    }
    Ok(Self {
        project_slug: project_slug.to_string(),
        native_session_id: native_session_id.to_string(),
        native_workflow_id: native_workflow_id.to_string(),
    })
}
```

Negative fixture: `adapter.rs:4089` uses `project/{SESSION}/subagents/workflows/wf_/journal.jsonl` as an *invalid* journal (empty suffix after `wf_`), and `adapter.rs:4168` uses `.../wf_main/events.jsonl` as a non-matching file.

### 1.6 `workflows/{runId}.json` in Path A

`project_parser.rs:763-821`, quoted in the load-bearing part:

```rust
/// Parse workflow run records under `projects/{slug}/{sid}/workflows/`.
fn read_workflows(project_dir: &Path, session_id: &str) -> Vec<WorkflowRun> {
    let dir = project_dir.join(session_id).join("workflows");
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with("wf_") || !name.ends_with(".json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };

        let workflow_id = data
            .get("runId")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| name.trim_end_matches(".json").to_owned());
        let num = |k: &str| data.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let name_field = data
            .get("workflowName")
            .and_then(|v| v.as_str())
            .unwrap_or(&workflow_id)
            .to_owned();
        let status = data
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let subagent_count = count_workflow_subagents(project_dir, session_id, &workflow_id) as f64;
        let journal = read_workflow_journal(project_dir, session_id, &workflow_id);

        out.push(WorkflowRun {
            name: name_field,
            status,
            agent_count: num("agentCount"),
            total_tokens: num("totalTokens"),
            total_tool_calls: num("totalToolCalls"),
            duration_ms: num("durationMs"),
            subagent_count,
            data,
            journal,
            workflow_id,
        });
    }

    out.sort_by(|a, b| a.workflow_id.cmp(&b.workflow_id));
    out
}
```

So the workflow id comes from the **JSON body's `runId`**, with the filename stem as fallback. (Path B instead *rejects* the document when `runId != filename` — `sidecars.rs:994-1001`.)

Recount of nested agents — `project_parser.rs:823-841`:

```rust
fn count_workflow_subagents(project_dir: &Path, session_id: &str, workflow_id: &str) -> usize {
    let dir = project_dir
        .join(session_id)
        .join("subagents")
        .join("workflows")
        .join(workflow_id);
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|n| n.starts_with("agent-") && n.ends_with(".jsonl"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}
```

Journal read — `project_parser.rs:843-857`:

```rust
fn read_workflow_journal(project_dir: &Path, session_id: &str, workflow_id: &str) -> Vec<Value> {
    let path = project_dir
        .join(session_id)
        .join("subagents")
        .join("workflows")
        .join(workflow_id)
        .join("journal.jsonl");
    let mut out = Vec::new();
    let _ = read_jsonl_streaming(&path, 0, |line, _idx, _off| {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            out.push(v);
        }
    });
    out
}
```

Untyped `Vec<Value>`; unparseable lines silently dropped; read errors swallowed by `let _ =`.

### 1.7 Other artifact patterns in Path A

| Artifact | Location | Pattern | Code |
|---|---|---|---|
| session JSONL | `projects/<slug>/` | `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$` | `project_parser.rs:42-45` |
| tool results | `projects/<slug>/<sid>/tool-results/` | `*.txt`, `tool_use_id` = stem | `:888-913` |
| file history | `<root>/file-history/<sid>/` | `^([0-9a-f]+)@v(\d+)$` | `:54-56`, `:917-968` |
| todos | `<root>/todos/` | prefix `"{session_id}-agent-"` + `.json`, then `^(.+?)-agent-(.+)\.json$` | `:60-61`, `:972-1013` |
| tasks | `<root>/tasks/<sid>/.lock`, `.highwatermark` | existence + `parseInt`-lenient | `:1017-1040` |
| plans | `<root>/plans/*.md` | title = first `(?m)^#\s+(.+)$` | `:1222-1275` |
| memory | `projects/<slug>/memory/MEMORY.md` | fixed path | `:640-643` |

The three static regexes at the head of the file — `project_parser.rs:40-61`:

```rust
/// Matches canonical session file names `<uuid>.jsonl` — identical to the
/// `UUID_JSONL` regex in `discoverSessionEntries`.
static UUID_JSONL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$")
        .expect("UUID_JSONL regex compiles")
});

/// Matches `agent-<id>.jsonl` where `<id>` starts with `a`. Port of
/// `extractAgentId`'s `^agent-(a.+)\.jsonl$` pattern.
static SUBAGENT_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^agent-(a.+)\.jsonl$").expect("SUBAGENT_FILE regex compiles"));

/// Matches file-history snapshot file names `<hash>@v<version>`. Port of
/// `parseFileHistory`'s `^([0-9a-f]+)@v(\d+)$` pattern.
static FILE_HISTORY_SNAPSHOT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^([0-9a-f]+)@v(\d+)$").expect("FILE_HISTORY_SNAPSHOT regex compiles")
});

/// Matches todo file names `<session-id>-agent-<agent-id>.json`. Port of
/// `parseTodos`'s `^(.+?)-agent-(.+)\.json$` pattern.
static TODO_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(.+?)-agent-(.+)\.json$").expect("TODO_FILE regex compiles"));
```

Warm-start fingerprint categories — `claude/fingerprint.rs:30-40` documents the taxonomy:

```rust
//! | category        | path shape                                                       |
//! |-----------------|------------------------------------------------------------------|
//! | `session`       | `projects/<slug>/<uuid>.jsonl`                                   |
//! | `subagent`      | `projects/<slug>/<session>/subagents/agent-*.jsonl`              |
//! | `tool_result`   | `projects/<slug>/<session>/tool-results/*.txt`                   |
//! | `memory`        | `projects/<slug>/memory/MEMORY.md`                               |
//! | `sessions_index`| `projects/<slug>/sessions-index.json`                            |
//! | `todo`          | `todos/<session>-agent-<agent>.json`                             |
//! | `task`          | `tasks/<session>/.lock` or `tasks/<session>/.highwatermark`      |
//! | `file_history`  | `file-history/<session>/<hash>@v<n>`                             |
```

with the two subagent-adjacent categories added later — `fingerprint.rs:79-82`:

```rust
/// `projects/<slug>/<session>/subagents/**/agent-*.meta.json`.
pub const CATEGORY_SUBAGENT_META: &str = "subagent_meta";
/// `projects/<slug>/<session>/subagents/workflows/<wf>/journal.jsonl`.
pub const CATEGORY_WORKFLOW_JOURNAL: &str = "workflow_journal";
```

and the reason, `fingerprint.rs:608-616`:

```rust
/// `subagents/` dir — flat `agent-*.jsonl`, their `.meta.json` sidecars, and
/// the nested `workflows/<wf_id>/` tree.
///
/// The parser reads all four shapes (`read_one_subagent` opens the sibling
/// `.meta.json`; `read_workflow_journal` opens `journal.jsonl`; the nested walk
/// picks up workflow-orchestrated transcripts). Discovery previously saw only
/// the flat `agent-*.jsonl`, so a changed agent type, a changed journal, or a
/// whole workflow's transcripts were invisible to warm start — RFC 008
/// Phase 1.4.
```

The fingerprint walk mirrors the parser — `fingerprint.rs:617-657`:

```rust
fn scan_subagents(
    dir: &Path, slug: &str, session_id: &str, out: &mut Discovery,
) -> Result<(), FingerprintError> {
    scan_agent_files(dir, slug, session_id, out)?;

    // Nested: subagents/workflows/<wf_id>/{agent-*.jsonl, agent-*.meta.json,
    // journal.jsonl}. Mirrors the parser's walk in `read_subagents`.
    let workflows = dir.join("workflows");
    if let Some(wf_dirs) = read_dir_reporting(&workflows, out) {
        for wf_entry in wf_dirs {
            ...
            scan_agent_files(&wf_path, slug, session_id, out)?;
            let journal = wf_path.join("journal.jsonl");
            if journal.is_file() {
                push_file(&mut out.files, &journal, CATEGORY_WORKFLOW_JOURNAL,
                    Some(slug.to_owned()), Some(session_id.to_owned()))?;
            }
        }
    }
    Ok(())
}
```

Session-directory gating uses a bare-UUID regex — `fingerprint.rs:572-598`:

```rust
// Session-subdir names are UUIDs; derive by stripping `.jsonl` off the
// `UUID_JSONL` pattern, i.e. they match the same hex-dashed shape.
static UUID_BARE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        .expect("UUID_BARE regex compiles")
});
...
if !UUID_BARE.is_match(name) { continue; }
let session_id = name.to_owned();
scan_subagents(&path.join("subagents"), slug, &session_id, out)?;
scan_tool_results(&path.join("tool-results"), slug, &session_id, out)?;
```

Note the fingerprint walk **does** gate session directories on the UUID shape, while `read_subagents` in Path A does not (it is only ever called with a session id that came from the index/discovery, so the gating is implicit).

---

## 2. How subagent transcripts DIFFER from main transcripts in parsing

**Yes, the same `SessionMessage` enum is used** — but that is where the similarity ends. The two go through completely different functions with different failure semantics.

### Main session line

`project_parser.rs:309-356` streams the file and calls `build_message_event` per line:

```rust
let stream_result = read_jsonl_streaming(&file_path, 0, |line, index, byte_offset| {
    if send_error.is_some() {
        // A previous send failed; skip further work. The outer loop
        // will still run but do nothing.
        return;
    }
    match build_message_event(slug, &session_id, line, index, byte_offset) {
        Ok((ev, lost_fts_text)) => {
            if let Err(e) = events.send(ev) { send_error = Some(e); return; }
            message_count = message_count.saturating_add(1);
            last_byte_position = byte_offset;

            // The row is stored, but its searchable text is not. Report it
            // so the failure is visible and the fingerprint is withheld —
            // an empty `text_content` is otherwise indistinguishable from a
            // message that genuinely has nothing to index, which is how a
            // `Vec<String>` that should have been numbers cost 21 user
            // messages their place in the index without moving any count.
            if let Some(detail) = lost_fts_text {
                if let Err(e) = events.send(IngestEvent::RecordSkip {
                    slug: slug.to_owned(),
                    path: file_path.to_string_lossy().into_owned(),
                    message: format!(
                        "session {session_id} line {index}: stored without searchable text — {detail}"
                    ),
                }) { send_error = Some(e); }
            }
        }
        Err(parse_err) => {
            // Skip the record, keep the project. The path is the session
            // file, which is what the fingerprint suppression keys on, so
            // the file is re-read next run rather than being recorded as
            // successfully ingested.
            if let Err(e) = events.send(IngestEvent::RecordSkip {
                slug: slug.to_owned(),
                path: file_path.to_string_lossy().into_owned(),
                message: format!("session {session_id} line {index}: {parse_err}"),
            }) { send_error = Some(e); }
        }
    }
});
```

`build_message_event` — `project_parser.rs:451-485`:

```rust
/// Parse one JSONL line into an `IngestEvent::Message` via the Claude
/// [`message_extractor`] (RFC 006 / Phase B seam).
///
/// The second element is set when the line's typed parse failed on a variant
/// that should have contributed searchable text. The row is still returned —
/// the verbatim JSON belongs in the table either way — and the caller reports
/// the loss separately.
fn build_message_event(
    slug: &str, session_id: &str, line: &str, index: u32, byte_offset: u64,
) -> Result<(IngestEvent, Option<String>), serde_json::Error> {
    let p = message_extractor::project_jsonl_line(line)?;
    let lost = p.lost_fts_text;
    Ok((
        IngestEvent::Message {
            slug: slug.to_owned(),
            session_id: session_id.to_owned(),
            index,
            byte_offset,
            raw_json: line.to_owned(),      // <-- VERBATIM
            msg_type: p.msg_type,
            uuid: p.uuid,
            timestamp: p.timestamp,
            input_tokens: p.input_tokens,
            output_tokens: p.output_tokens,
            cache_creation_tokens: p.cache_creation_tokens,
            cache_read_tokens: p.cache_read_tokens,
            fts_text: p.fts_text,
        },
        lost,
    ))
}
```

The extractor — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/message_extractor.rs:41-111`:

```rust
/// Project one JSONL line into stored columns.
///
/// Always returns `Ok` for valid JSON (including unknown `type` values).
/// Returns `Err` only when the line is not JSON at all — matching the
/// parser behaviour of emitting `RecordSkip` for bad lines.
///
/// A *typed* parse failure is not an `Err` here: the row still belongs in the
/// table with its verbatim JSON. It surfaces as [`MessageProjection::lost_fts_text`]
/// when it cost content, so the caller can report it without discarding the row.
pub fn project_jsonl_line(line: &str) -> Result<MessageProjection, serde_json::Error> {
    // Loose Value first for top-level fields (matches TS
    // `msg as Record<string, unknown>`), so we don't reverse-engineer
    // serde tag renames for `msg_type`.
    let value: Value = serde_json::from_str(line)?;
    let typed = serde_json::from_str::<SessionMessage>(line);
    Ok(project_parsed_line(&value, &typed))
}

pub fn project_parsed_line(
    value: &Value,
    typed: &Result<SessionMessage, serde_json::Error>,
) -> MessageProjection {
    let msg_type = value.get("type").and_then(Value::as_str).unwrap_or("unknown").to_owned();
    let uuid = value.get("uuid").and_then(Value::as_str).map(str::to_owned);
    let timestamp = value.get("timestamp").and_then(Value::as_str).map(str::to_owned);

    let (input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens) =
        if msg_type == "assistant" { extract_tokens(value) } else { (0, 0, 0, 0) };

    // Typed parse for FTS; failure → still emit the row with no fts blob, but
    // say so when the blob should not have been empty.
    let (fts_text, lost_fts_text) = match typed {
        Ok(msg) => {
            let s = fts_text::extract_message_text(msg);
            (if s.is_empty() { None } else { Some(s) }, None)
        }
        Err(e) if fts_text::type_contributes_text(&msg_type) => {
            (None, Some(format!("type={msg_type}: {e}")))
        }
        Err(_) => (None, None),
    };
    ...
}
```

Key properties of the main path: the **loose `Value` is authoritative** for `msg_type`/`uuid`/`timestamp`; the typed parse is used *only* for FTS; the raw line is stored byte-for-byte; a typed failure never loses the row.

### Subagent line

`project_parser.rs:700-761`, the whole of it:

```rust
/// Lines a subagent transcript could not type-parse, and that carried content.
///
/// A dropped line here is worse than on the session path — the transcript is
/// the only thing holding it, so there is no verbatim JSON to fall back on.
/// Even so the filter is the same [`fts_text::type_contributes_text`], because
/// the alternative is untenable: a transcript containing a `{}` line, or any of
/// the skills telemetry that shares these files, would withhold its fingerprint
/// and re-ingest on every warm start forever. The Phase 1 convergence gate
/// fails exactly that way if this reports indiscriminately.
struct DroppedSubagentLines {
    path: String,
    count: u32,
    first_error: String,
}

fn read_one_subagent(
    path: &Path,
    name: &str,
    workflow_id: &str,
) -> (SubagentTranscript, Option<DroppedSubagentLines>) {
    let mut messages: Vec<SessionMessage> = Vec::new();
    let mut dropped: Option<DroppedSubagentLines> = None;
    let _ = read_jsonl_streaming(path, 0, |line, idx, _off| {
        match serde_json::from_str::<SessionMessage>(line) {
            Ok(msg) => messages.push(msg),
            Err(e) => {
                let msg_type = serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_default();
                if !fts_text::type_contributes_text(&msg_type) {
                    return;
                }
                match &mut dropped {
                    Some(d) => d.count = d.count.saturating_add(1),
                    None => {
                        dropped = Some(DroppedSubagentLines {
                            path: path.to_string_lossy().into_owned(),
                            count: 1,
                            first_error: format!("line {idx} (type={msg_type}): {e}"),
                        })
                    }
                }
            }
        }
    });
    // Sibling `agent-{id}.meta.json` carries the real agent type + description.
    let meta = std::fs::read_to_string(path.with_extension("meta.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<SubagentMeta>(&raw).ok());
    (
        SubagentTranscript {
            agent_id: extract_agent_id(name),
            agent_type: infer_agent_type(name),
            file_name: name.to_owned(),
            messages,
            meta,
            workflow_id: workflow_id.to_owned(),
        },
        dropped,
    )
}
```

The concrete differences, enumerated:

**1. Typed parse is the ONLY parse, and it is load-bearing.** A line that fails `SessionMessage` deserialization is **gone** — there is no `raw_json` fallback, no `messages` row, nothing. The doc comment above says it directly: *"the transcript is the only thing holding it, so there is no verbatim JSON to fall back on."*

**2. Content is RE-SERIALIZED, not stored verbatim.** `core/writer.rs:1347-1371`:

```rust
for (index, message) in transcript.messages.iter().enumerate() {
    let value = serde_json::to_value(message)?;          // <-- round-trip through the typed enum
    let tokens = subagent_message_tokens(&value, source_id);
    let timestamp = value
        .get("timestamp")
        .and_then(|candidate| candidate.as_str())
        .map(str::to_owned);
    conn.execute(
        SQL_INSERT_SUBAGENT_MESSAGE,
        params![
            source_id,
            slug,
            session_id,
            transcript.workflow_id,
            transcript.agent_id,
            index as i64,
            timestamp,
            tokens[0], tokens[1], tokens[2], tokens[3],
            serde_json::to_string(&value)?,             // <-- re-encoded, not the original line
        ],
    )?;
}
```

`subagent_messages.data` is therefore the **serde re-encode of the typed struct**, so any JSON key the enum does not model is dropped. Field ordering and formatting are serde's, not the file's. The timestamp is re-read *from that re-serialized value*, not from the original line.

Two `#[serde(flatten)] extra` escape hatches limit the damage — `claude/types/session.rs:392-404`:

```rust
/// System messages carry a `subtype` that further discriminates them, but the
/// only field the ingest pipeline reads is the top-level `content` prose (for
/// FTS, regardless of subtype — matching the TS extractor). We therefore keep
/// `base` / `level` / `is_meta` typed and stash everything else (`subtype`,
/// `content`, and any subtype-specific payload) verbatim in `extra`.
///
/// This deliberately does NOT model `subtype` as an internally-tagged enum:
/// such an enum requires the `subtype` field to be present, so a system line
/// with a *missing* `subtype` failed the whole parse (and `#[serde(other)]`
/// only catches unknown *values*, never absence) — nulling the line's FTS
/// text. The flat shape parses a missing or unknown subtype without error and
/// round-trips the extra keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessage {
    #[serde(flatten)]
    pub base: BaseMessageFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,
    /// `subtype`, `content`, and any subtype-specific fields, kept verbatim so
    /// re-serialization (subagent transcripts) preserves them.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}
```

— note *"kept verbatim so re-serialization (subagent transcripts) preserves them"*: the comment exists precisely because of this path. Same for `AttachmentPayload.extra` at `session.rs:213-214`.

**3. Token extraction is separate code** — `core/writer.rs:1151-1171`:

```rust
fn subagent_message_tokens(value: &serde_json::Value, source_id: &str) -> [i64; 4] {
    if source_id != "claude-code" || value.get("type").and_then(|v| v.as_str()) != Some("assistant")
    {
        return [0, 0, 0, 0];
    }
    let usage = value
        .get("message")
        .and_then(|message| message.get("usage"));
    let number = |key: &str| {
        usage
            .and_then(|candidate| candidate.get(key))
            .and_then(|candidate| candidate.as_u64())
            .unwrap_or(0) as i64
    };
    [
        number("input_tokens"),
        number("output_tokens"),
        number("cache_creation_input_tokens"),
        number("cache_read_input_tokens"),
    ]
}
```

Same four keys as the main path (`message_extractor.rs:113-124`), but reached through the re-serialized value and additionally gated on `source_id == "claude-code"`.

**4. No FTS.** `subagent_messages` has no `text_content` column — `core/schema.rs:358-373`:

```sql
CREATE TABLE IF NOT EXISTS subagent_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  workflow_id TEXT NOT NULL DEFAULT '',
  agent_id TEXT NOT NULL,
  msg_index INTEGER NOT NULL,
  timestamp TEXT,
  input_tokens INTEGER DEFAULT 0,
  output_tokens INTEGER DEFAULT 0,
  cache_creation_tokens INTEGER DEFAULT 0,
  cache_read_tokens INTEGER DEFAULT 0,
  data TEXT NOT NULL,
  UNIQUE(source_id, session_id, workflow_id, agent_id, msg_index)
);
```

The main `messages` table does have `text_content` (bound at `core/writer.rs:1258`).

**5. No `RecordSkip` for malformed JSON** — only for typed failures on content-bearing types. The filter — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/fts_text.rs:157-162`:

```rust
pub fn type_contributes_text(msg_type: &str) -> bool {
    matches!(
        msg_type,
        "user" | "assistant" | "system" | "summary" | "ai-title"
    )
}
```

The rationale and its counterweight are pinned by tests — `project_parser.rs:1440-1505`:

```rust
#[test]
fn subagent_transcript_reports_a_dropped_message_but_not_a_dropped_blank() {
    // A dropped line is worse here than on the session path — the
    // transcript is the only thing holding it, so it is simply gone and
    // `message_count` is quietly short. But the filter still has to be
    // "did this carry content", because `{}` lines and skills telemetry
    // live in these files too and reporting those would withhold the
    // fingerprint on every warm start (the Phase 1 convergence gate fails
    // exactly that way).
    ...
    // Transcript A: a `user` line serde rejects → real loss, reported.
    // Transcript B: a bare `{}` → carries nothing, must stay silent.
    ...
    assert_eq!(skips.len(), 1, "exactly one transcript lost content");
```

**6. Read errors are swallowed entirely** — `let _ = read_jsonl_streaming(...)` at `project_parser.rs:722`. The main-session path checks `stream_result` and emits a `RecordSkip` — `project_parser.rs:362-379`:

```rust
match stream_result {
    Ok(r) => {
        // read_jsonl_streaming reports the final byte position past the
        // last byte read, even if no complete lines were yielded.
        last_byte_position = r.final_byte_position.max(last_byte_position);
    }
    Err(e) => {
        // A mid-read failure costs this session, not the project: the
        // messages already streamed above are real and other sessions in
        // the project are unaffected. Withholding the fingerprint is what
        // makes the truncated read retry.
        events.send(IngestEvent::RecordSkip {
            slug: slug.to_owned(),
            path: file_path.to_string_lossy().into_owned(),
            message: format!("session {session_id} read error: {e}"),
        })?;
    }
}
```

**7. `message_count` is `transcript.messages.len()`** — `core/writer.rs:1302` — the count of *successfully typed* lines, not lines in the file. A dropped line silently shortens it.

**8. Empty transcripts get a dirty-thread marker** rather than being dropped — `core/writer.rs:1372-1387`:

```rust
if transcript.messages.is_empty() {
    conn.execute(
        r#"INSERT INTO subagent_dirty_threads
           (source_id, project_slug, session_id, workflow_id, agent_id)
           VALUES (?, ?, ?, ?, ?)
           ON CONFLICT(source_id, session_id, workflow_id, agent_id)
           DO UPDATE SET project_slug = excluded.project_slug"#,
        params![source_id, slug, session_id, transcript.workflow_id, transcript.agent_id],
    )?;
}
```

**9. Subagent messages are deleted-then-reinserted** on every ingest — `core/writer.rs:1338-1346`:

```rust
conn.execute(
    SQL_DELETE_SUBAGENT_MESSAGES,
    params![source_id, session_id, transcript.workflow_id, transcript.agent_id],
)?;
```
```sql
-- core/writer.rs:266-269
DELETE FROM subagent_messages
WHERE source_id = ? AND session_id = ? AND workflow_id = ? AND agent_id = ?
```

versus the main `messages` table, which upserts per-row on `byte_offset`/index.

### Record types that appear

Same enum for both — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/session.rs:18-52`:

```rust
/// One line of a session JSONL file. TS `SessionMessage`.
///
/// Uses `#[serde(tag = "type")]` to dispatch on the `type` field. Unknown
/// types would fail to deserialize — callers in the ingest pipeline wrap
/// per-line parsing in error handlers, so that's acceptable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SessionMessage {
    AgentName(AgentNameMessage),
    Attachment(AttachmentMessage),
    CustomTitle(CustomTitleMessage),
    FileHistorySnapshot(FileHistorySnapshotMessage),
    PrLink(PrLinkMessage),
    Progress(ProgressMessage),
    PermissionMode(PermissionModeMessage),
    #[serde(rename = "saved_hook_context")]
    SavedHookContext(SavedHookContextMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    #[serde(rename = "system")]
    System(SystemMessage),
    #[serde(rename = "summary")]
    Summary(SummaryMessage),
    QueueOperation(QueueOperationMessage),
    LastPrompt(LastPromptMessage),
    AiTitle(AiTitleMessage),
    Mode(ModeMessage),
    BridgeSession(BridgeSessionMessage),
    /// Any `type` value this build doesn't model. Without this backstop
    /// a message type newly introduced by Claude Code fails the typed
    /// parse — which nulls the line's FTS text (the raw JSONL line is
    /// still stored verbatim; only the typed projection is a no-op).
    #[serde(other)]
    Unknown,
}
```

In practice subagent transcripts carry `user` / `assistant` / `system` / `progress`. **Two regressions were fixed specifically because they hurt subagents:**

`AssistantMessage.request_id` — `session.rs:352-359`:

```rust
pub struct AssistantMessage {
    #[serde(flatten)]
    pub base: BaseMessageFields,
    /// Absent on some API-error assistant lines; a missing `requestId` must
    /// not fail the typed parse (which would null FTS + drop the line from
    /// subagent transcripts). Defaults to an empty string.
    #[serde(default)]
    pub request_id: String,
```

with the regression test — `project_parser.rs:1849-1888`:

```rust
// ── 12. api-error assistant kept in subagent transcript (item 1) ──────

#[test]
fn subagent_keeps_assistant_line_without_usage_or_request_id() {
    // An assistant transcript line without `requestId` / `message.usage`
    // (API-error line) previously failed the typed parse and was dropped
    // from the subagent's `messages`, undercounting message_count. It must
    // now be retained.
    ...
    assert_eq!(
        transcript.messages.len(), 1,
        "api-error assistant line must not be dropped from the transcript"
    );
}
```

`UserMessage.image_paste_ids` — `session.rs:291-300`:

```rust
/// Never read — carried only to mirror the JSONL shape.
///
/// Declared `string[]` in `packages/sdk/src/types/claude/projects.ts`, but
/// Claude Code actually writes **numbers** (`"imagePasteIds": [1]`). TS
/// erases its types at runtime so the wrong annotation costs it nothing;
/// serde is strict, so `Vec<String>` failed the whole `user` record and
/// silently emptied its FTS blob and dropped it from subagent transcripts.
/// Untyped `Value` because an unread field must never fail a record.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub image_paste_ids: Option<Value>,
```

Also relevant: `ToolUseResult` was widened for the same class of reason — `session.rs:330-344`:

```rust
/// TS `toolUseResult?: string | object`.
///
/// The object half varies per tool — Read emits `{type, file:{...}}`,
/// Bash emits `{stdout, stderr, interrupted, ...}`, Edit emits diff
/// shapes, and so on. Nothing downstream introspects it, so it stays
/// opaque. An earlier strict struct here (Read's shape only) made typed
/// deserialization fail for ANY session line whose tool result didn't
/// match it, which nulled that message's FTS text — search over tool
/// output silently missed on the Rust engine (2026-07 audit).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolUseResult {
    Text(String),
    Object(Value),
}
```

### Path B: no parsing difference at all

`adapter.rs:722-758` routes both decoders into the **same** `decode_transcript_record`; the only distinction is a guard on `agent_id` presence:

```rust
fn decode(
    &self,
    context: DecodeContext<'_>,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    match context.decoder.as_str() {
        PARENT_DECODER => {
            let object_context = ClaudeTranscriptContext::decode(context.object_context)?;
            if object_context.agent_id.is_some() {
                return Err(decoder_context_mismatch());
            }
            decode_transcript_record(
                self.adapter_id(), &object_context, context.decoder_state, record, output,
            )
        }
        SUBAGENT_DECODER => {
            let object_context = ClaudeTranscriptContext::decode(context.object_context)?;
            if object_context.agent_id.is_none() {
                return Err(decoder_context_mismatch());
            }
            decode_transcript_record(
                self.adapter_id(), &object_context, context.decoder_state, record, output,
            )
        }
        SUBAGENT_META_DECODER => {
            let object_context = ClaudeSubagentMetadataContext::decode(context.object_context)?;
            decode_subagent_metadata(self.adapter_id(), &object_context.child, record, output)
        }
        ...
```

In Path B the raw bytes **are** kept — `adapter.rs:2018-2034`:

```rust
output.push(
    record,
    Fact::Message(MessageFact {
        message: message.clone(),
        session: session.clone(),
        run: run.clone(),
        native_message_id,
        native_kind: message_native_kind,
        role,
        content,
        source_time: source_time.clone(),
        parent_native_message_id,
        model: model.clone(),
        search_text: projection.fts_text,
        raw_json: record.payload.clone(),     // <-- verbatim bytes
    }),
)?;
```

and both parses are done once and shared — `adapter.rs:1827-1830`:

```rust
// One loose parse and one typed parse per record, shared by the projection
// and every fact family below. Each of these used to be done twice.
let typed = serde_json::from_str::<SessionMessage>(raw);
let projection = message_extractor::project_parsed_line(&value, &typed);
```

Two behavioural branches inside remain keyed on `agent_id`:

- Session-metadata projection (first prompt / titles) is **skipped for subagents** — `adapter.rs:1846-1850`:
  ```rust
  let metadata = if context.agent_id.is_none() {
      session_metadata::project_session_metadata(&value).unwrap_or_default()
  } else {
      session_metadata::SessionMetadataProjection::default()
  };
  ```
- A typed-parse failure emits a scoped diagnostic rather than dropping the record — `adapter.rs:1969-1992`:
  ```rust
  let (role, content, parent_native_message_id, model) = match &typed {
      Ok(message) => decode_message_content(message, &value),
      Err(error) => {
          output.push_scoped_diagnostic(
              AdapterDiagnostic {
                  class: AdapterErrorClass::RecordPermanent,
                  code: "claude_typed_projection_loss".to_string(),
                  message: format!("type={}: {error}", projection.msg_type),
              },
              capability_ids(&[HISTORY_MESSAGES, HISTORY_CONTENT_BLOCKS, RUNTIME_SUBAGENTS]),
          )?;
          (
              role_from_kind(&projection.msg_type),
              Vec::new(),
              nonempty_field(&value, "parentUuid"),
              value.get("message").and_then(|m| m.get("model"))
                  .and_then(Value::as_str).filter(|m| !m.is_empty()).map(str::to_owned),
          )
      }
  };
  ```

Message identity keys include the workflow and agent — `adapter.rs:2685-2712`:

```rust
fn message_native_key(
    context: &ClaudeTranscriptContext,
    record: &SourceRecord,
    native_message_id: Option<&str>,
) -> Vec<u8> {
    let mut key = Vec::new();
    push_key_component(&mut key, context.session_id.as_bytes());
    push_key_component(&mut key, context.workflow_id.as_deref().unwrap_or("").as_bytes());
    push_key_component(&mut key, context.agent_id.as_deref().unwrap_or("").as_bytes());
    if let Some(native_message_id) = native_message_id {
        push_key_component(&mut key, native_message_id.as_bytes());
    } else {
        // Catalog row IDs and generations are observation provenance, not
        // native identity. Scheduler order can allocate a different object ID
        // on a fresh build, and a rewrite necessarily advances generation.
        // The object context above plus the framed byte range is stable for
        // the same native transcript content across both cases.
        push_key_component(&mut key, record.cursor_start.as_bytes());
        push_key_component(&mut key, record.cursor_end.as_bytes());
    }
    key
}
```

---

## 3. Sidechain handling and parent linkage

### 3.1 `isSidechain` — carried, and used for exactly one thing

It is modelled in two places:

`BaseMessageFields.is_sidechain` — `claude/types/session.rs:58-86`:

```rust
/// Fields shared by JSONL lines that live inside a threaded conversation.
/// TS `BaseMessageFields`. Included via `#[serde(flatten)]` in the variants
/// that need it (user, assistant, attachment, progress, system, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BaseMessageFields {
    pub uuid: String,
    #[serde(default)]
    pub parent_uuid: Option<String>,
    pub timestamp: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub git_branch: String,
    #[serde(default)]
    pub is_sidechain: bool,
    /// TS literal `'external'`; kept as `String` for forward compat.
    #[serde(default)]
    pub user_type: String,
    ...
}
```

and `SessionIndexEntry.is_sidechain` — `claude/types/project.rs:55-56`.

**The only place it changes behaviour in this crate** is first-prompt/title selection — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/session_metadata.rs:57-75`:

```rust
pub fn extract_human_prompt(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) != Some("user")
        || record.get("isMeta").and_then(Value::as_bool) == Some(true)
        || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        || record
            .get("isVisibleInTranscriptOnly")
            .and_then(Value::as_bool)
            == Some(true)
    {
        return None;
    }

    let text = first_text_content(record)?;
    if is_synthetic_prompt_text(&text) {
        return None;
    }
    Some(crate::core::text::truncate_utf16(&text, 200).to_owned())
}
```

i.e. a sidechain user line can never become a session's `first_prompt` or title. (It feeds both `peek_first_user_prompt` at `project_parser.rs:1199-1215` and `project_session_metadata` at `session_metadata.rs:77-99`.)

Everywhere else `is_sidechain` is only **stored and projected**, never used to group or filter:

- `sessions.is_sidechain` column — `core/schema.rs:279`, written at `core/writer.rs:1215` (`entry.is_sidechain as i64`).
- `session_index_entry_assertions.is_sidechain` / `canonical_session_index_entries.is_sidechain` — `core/schema.rs:878`, `:913`; written at `engine/session_index_projection.rs:249` (`i64::from(entry.is_sidechain)`) and carried through the reduction SELECT at `:471-474` and `:492`.
- Read back for display at `engine/history_query.rs:486`, `:508`, `:587-588`; `engine/detail_query.rs:111`, `:420`, `:494`; `engine/query_pool.rs:260`; `adapter/facts.rs:343`.
- The Codex and Grok readers hard-code `is_sidechain: false` (`codex/reader.rs:464`, `grok/reader.rs:377`), which is only possible because nothing downstream depends on it being meaningful.

**There is no "group the sidechain lines of the main transcript into a subagent thread" logic anywhere in this crate.** Sidechain grouping is not how subagents are reconstructed — the filesystem layout is (`subagents/agent-*.jsonl`), plus the linkage rules below. If someone is porting logic that assumed sidechain-based grouping, that assumption does not hold here.

### 3.2 Linking a subagent to its parent — Path A: content-match on `tool_result`

`core/writer.rs:1090-1149`, quoted in full because it is the entire linkage mechanism on this path:

```rust
/// Find the `tool_use_id` of the tool call that spawned a subagent.
///
/// Port of the TS `resolveSubagentSpawnToolId`. Claude records a subagent's
/// result as a `tool_result` block in the parent session whose content
/// mentions the agent id; that block's `tool_use_id` is the spawning call.
/// Without this the whole `subagents.spawn_tool_id` / `link_method` pair was
/// written as `NULL` / `"unlinked"` literals — on a real corpus that silently
/// dropped the linkage for 113 of 638 subagents (RFC 008 Phase 5).
///
/// The `LIKE` is an optimisation, not the test. TS re-reads and JSON-parses
/// every message in the session for *every* subagent, which is quadratic in a
/// session with many agents; pushing a substring prefilter into SQLite means
/// only plausible rows are parsed. It is a strict superset of the JSON check
/// below, so it cannot cause a miss.
fn resolve_spawn_tool_id(
    conn: &Connection,
    source_id: &str,
    session_id: &str,
    agent_id: &str,
) -> Result<Option<String>, WriterError> {
    if agent_id.is_empty() {
        return Ok(None);
    }
    let mut stmt = conn.prepare_cached(
        "SELECT data FROM messages          WHERE session_id = ?1 AND source_id = ?2 AND data LIKE '%' || ?3 || '%'          ORDER BY msg_index",
    )?;
    let mut rows = stmt.query(params![session_id, source_id, agent_id])?;
    while let Some(row) = rows.next()? {
        let data: String = row.get(0)?;
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue; // malformed raw row — TS swallows these too
        };
        let Some(content) = raw
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }
            let Some(tool_use_id) = block.get("tool_use_id").and_then(|t| t.as_str()) else {
                continue;
            };
            // The block's content may be a bare string or a nested structure;
            // stringify either way, matching TS.
            let body = match block.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            if body.contains(agent_id) {
                return Ok(Some(tool_use_id.to_owned()));
            }
        }
    }
    Ok(None)
}
```

Line **`core/writer.rs:1143`** is the actual test: `if body.contains(agent_id)`.

Consumed at `core/writer.rs:1286-1337`:

```rust
IngestEvent::Subagent {
    slug,
    session_id,
    transcript,
} => {
    let now = now_ms();
    // Prefer the sidecar's real agent type (general-purpose, Explore, …)
    // over the filename-inferred enum kind. `to_string` on the enum
    // produces `"task"` (with quotes) — strip them to store the bare
    // string, matching the TS convention.
    let agent_type = match transcript.meta.as_ref() {
        Some(meta) => meta.agent_type.clone(),
        None => serde_json::to_string(&transcript.agent_type)?
            .trim_matches('"')
            .to_string(),
    };
    let message_count = transcript.messages.len() as i64;
    // Only worktree-isolated agents carry this; everything else ran in
    // the project root and stores NULL rather than a redundant copy.
    let worktree_path = transcript
        .meta
        .as_ref()
        .and_then(|meta| meta.worktree_path.clone());
    // Resolved here rather than in the parser: the linkage lives in
    // the *parent session's* messages, which are already written by
    // the time a subagent event arrives (the parser streams a
    // session's messages before reading its subagents) and are
    // visible to this same transaction.
    let spawn_tool_id =
        resolve_spawn_tool_id(conn, source_id, session_id, &transcript.agent_id)?;
    let link_method = if spawn_tool_id.is_some() {
        "tool_result"
    } else {
        "unlinked"
    };
    conn.execute(
        SQL_INSERT_SUBAGENT,
        params![
            source_id,
            slug,
            session_id,
            transcript.agent_id,
            agent_type,
            transcript.file_name,
            message_count,
            transcript.workflow_id,
            spawn_tool_id,
            link_method,
            worktree_path,
            now,
        ],
    )?;
```

**`link_method` has exactly two values: `"tool_result"` and `"unlinked"`.** The schema default is `"unlinked"` — `core/schema.rs:337-356`:

```sql
CREATE TABLE IF NOT EXISTS subagents (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL,
  project_slug TEXT NOT NULL,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  agent_type TEXT NOT NULL,
  file_name TEXT NOT NULL,
  message_count INTEGER NOT NULL,
  workflow_id TEXT NOT NULL DEFAULT '',
  spawn_tool_id TEXT,
  link_method TEXT NOT NULL DEFAULT 'unlinked',
  -- Absolute path of the git worktree the agent ran in, from the meta
  -- sidecar's `worktreePath`. NULL for the vast majority of agents, which
  -- run directly in the project root.
  worktree_path TEXT,
  updated_at INTEGER,
  UNIQUE(source_id, project_slug, session_id, workflow_id, agent_id)
);
```

The ordering guarantee that makes this work is stated in the comment above: the parser streams a session's messages (`project_parser.rs:309-356`) **before** reading its subagents (`project_parser.rs:381-400`), so `messages` rows are already visible inside the same transaction.

**Crucially, `sourceToolUseID` / `sourceToolAssistantUUID` are NOT used for this** — see §3.4.

### 3.3 Linking a subagent to its parent — Path B: directory layout + sidecar `toolUseId`

Path B links structurally. Run identity — `adapter.rs:1093-1103` (re-quoted for this section):

```rust
fn run_native_key(&self) -> String {
    match &self.agent_id {
        Some(agent_id) => format!(
            "{}\0{}\0{}",
            self.session_id,
            self.workflow_id.as_deref().unwrap_or(""),
            agent_id
        ),
        None => self.session_id.clone(),
    }
}
```

`sessionId \0 workflowId \0 agentId`, with an empty middle segment for non-workflow subagents; a root run's key is just the sessionId.

Parent run derived purely from presence of `agent_id` — `adapter.rs:1890-1900`:

```rust
let parent_run = if context.agent_id.is_some() {
    Some(EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "run",
        context.session_id.as_bytes(),
    )?)
} else {
    None
};
let delegation_parent = parent_run.clone();
```

Run + actor-run declaration — `adapter.rs:1901-1937`:

```rust
if !state.run_declared {
    output.push(
        record,
        Fact::Run(RunFact {
            run: run.clone(),
            session: session.clone(),
            native_run_id: run_native_key.clone(),
            parent_run,
        }),
    )?;
    let actor_run = canonical_transcript_actor_run(output, context)?;
    let canonical_session =
        output.canonical_entity_key("session", context.session_id.as_bytes())?;
    let parent_actor_run = context
        .agent_id
        .as_ref()
        .map(|_| output.canonical_root_actor_run_key(context.session_id.as_bytes(), None))
        .transpose()?;
    output.push_native(
        record,
        run_native_key.as_bytes(),
        Fact::ActorRunRevision(ActorRunRevisionFact {
            actor_run,
            session: canonical_session,
            role: if context.agent_id.is_some() {
                ActorRunRole::Child
            } else {
                ActorRunRole::Root
            },
            parent_actor_run,
            native_session_id: Some(context.session_id.clone()),
            native_actor_id: context.agent_id.clone(),
            native_actor_type: None,
        }),
    )?;
    state.run_declared = true;
}
```

Delegation fact — `adapter.rs:1938-1959`:

```rust
if let Some(agent_id) = &context.agent_id {
    if !state.delegation_declared || cwd_changed {
        output.push(
            record,
            Fact::Delegation(DelegationFact {
                child_run: run.clone(),
                parent_run: delegation_parent,
                session: session.clone(),
                kind: DelegationKind::VendorNativeSubagent,
                relation_strength: RelationStrength::Layout,   // <-- from directory structure
                native_child_id: Some(agent_id.clone()),
                native_task_id: None,
                label: None,
                prompt: None,
                cwd,
                worktree_path: None,
                source_time: source_time.clone(),
            }),
        )?;
        state.delegation_declared = true;
    }
}
```

Note `relation_strength: RelationStrength::Layout` and `native_task_id: None` — the layout alone establishes parentage but not *which tool call* did it.

The spawn side comes from **`Task` / `Agent` tool_use blocks in the parent transcript** — `adapter.rs:2480-2514`:

```rust
struct DelegationSpawnDescriptor {
    native_task_id: String,
    tool_name: String,
    label: Option<String>,
    prompt: Option<String>,
    requested_agent_type: Option<String>,
}

fn delegation_spawn_descriptors(content: &[ContentBlock]) -> Vec<DelegationSpawnDescriptor> {
    content
        .iter()
        .filter_map(|block| {
            let ContentBlock::ToolCall {
                native_id,
                name,
                input,
            } = block
            else {
                return None;
            };
            if !matches!(name.as_str(), "Task" | "Agent") || native_id.trim().is_empty() {
                return None;
            }
            Some(DelegationSpawnDescriptor {
                native_task_id: native_id.clone(),
                tool_name: name.clone(),
                label: nonempty_field(input, "description")
                    .or_else(|| nonempty_field(input, "name")),
                prompt: nonempty_field(input, "prompt"),
                requested_agent_type: nonempty_field(input, "subagent_type")
                    .or_else(|| nonempty_field(input, "agent_type")),
            })
        })
        .collect()
}
```

emitted as `Fact::DelegationSpawn` keyed `run_native_key ‖ native_task_id` — `adapter.rs:2057-2081`:

```rust
for descriptor in spawn_descriptors {
    let mut native_spawn_key = Vec::new();
    push_key_component(&mut native_spawn_key, run_native_key.as_bytes());
    push_key_component(&mut native_spawn_key, descriptor.native_task_id.as_bytes());
    output.push(
        record,
        Fact::DelegationSpawn(DelegationSpawnFact {
            spawn: EntityKey::native(
                adapter_id, record.source_instance_id, "delegation_spawn", &native_spawn_key,
            )?,
            parent_run: run.clone(),
            parent_message: message.clone(),
            session: session.clone(),
            native_task_id: descriptor.native_task_id,
            tool_name: descriptor.tool_name,
            label: descriptor.label,
            prompt: descriptor.prompt,
            requested_agent_type: descriptor.requested_agent_type,
            source_time: source_time.clone(),
        }),
    )?;
}
```

A real spawn fixture — `adapter.rs:6866`:

```
{"type":"assistant","uuid":"spawn-message","parentUuid":"u1",...,"message":{...,"content":[{"type":"tool_use","id":"tool-spawn-1","name":"Task","input":{"description":"Map the parser","prompt":"Inspect the implementation","subagent_type":"Explore"}}]}}
```

The **actual join** between "a spawn happened" and "this transcript is that child" is closed by the meta sidecar's `toolUseId` — `decode_subagent_metadata`, `adapter.rs:1718-1793`:

```rust
fn decode_subagent_metadata(
    adapter_id: &AdapterId,
    context: &ClaudeTranscriptContext,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let document: ClaudeSubagentMetadataDocument = match serde_json::from_slice(&record.payload) {
        Ok(document) => document,
        Err(error) => {
            preserve_unknown(
                record, output,
                Some("subagent_metadata".to_string()),
                format!("Claude subagent metadata is not a supported JSON object: {error}"),
            )?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let Some(agent_type) = nonempty(&document.agent_type) else {
        preserve_unknown(
            record, output,
            Some("subagent_metadata".to_string()),
            "Claude subagent metadata has an empty agentType".to_string(),
        )?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let agent_id = context.agent_id.as_deref().ok_or_else(|| {
        AdapterError::invalid_contract("subagent metadata context has no native child id")
    })?;
    let native_member_name = document.name.as_deref().and_then(nonempty);
    let native_team_id = document.team_name.as_deref().and_then(nonempty);
    let session = EntityKey::native(
        adapter_id, record.source_instance_id, "session", context.session_id.as_bytes(),
    )?;
    let run_native_key = context.run_native_key();
    let child_run = EntityKey::native(
        adapter_id, record.source_instance_id, "run", run_native_key.as_bytes(),
    )?;
    output.push(
        record,
        Fact::DelegationMetadata(DelegationMetadataFact {
            child_run,
            session,
            native_child_id: agent_id.to_string(),
            agent_type,
            description: document.description.as_deref().and_then(nonempty),
            name: native_member_name.clone(),
            spawn_depth: document.spawn_depth,
            worktree_path: document.worktree_path.as_deref().and_then(nonempty),
            native_task_id: document.tool_use_id.as_deref().and_then(nonempty),  // <-- THE JOIN KEY
        }),
    )?;
    if let (Some(native_team_id), Some(native_member_name)) =
        (native_team_id.as_deref(), native_member_name.as_deref())
    {
        emit_team_affiliation(
            record, output,
            ClaudeTeamAffiliationActor::Child {
                native_session_id: &context.session_id,
                run_native_key: &run_native_key,
            },
            native_team_id,
            Some(native_member_name),
            None,
        )?;
    }
    Ok(DecodeDisposition::Applied)
}
```

So Path B's chain is: **directory layout → `Delegation` (Layout strength)**, **parent `Task`/`Agent` tool_use → `DelegationSpawn` (`native_task_id` = tool_use id)**, **sidecar `toolUseId` → `DelegationMetadata.native_task_id`** which joins the two. No substring search, no content matching.

There is also a consistency check between actor role and context — `adapter.rs:917-919`:

```rust
|| (actor.role == ActorRunRole::Root) != context.agent_id.is_none()
|| (... && actor.native_actor_id.as_deref() != context.agent_id.as_deref())
```

And the canonical actor-run helper — `adapter.rs:2233-2242`:

```rust
fn canonical_transcript_actor_run(
    output: &FactBatch,
    context: &ClaudeTranscriptContext,
) -> Result<crate::adapter::CanonicalEntityKey, AdapterError> {
    if context.agent_id.is_some() {
        output.canonical_entity_key("run", context.run_native_key().as_bytes())
    } else {
        output.canonical_root_actor_run_key(context.session_id.as_bytes(), None)
    }
}
```

A parallel derivation for todo documents shows the same convention — `adapter.rs:2244-2262`:

```rust
/// Actor that owns a `todos/<session>-agent-<agent>.json` document.
///
/// The file name names its owner: an agent equal to the session is the root
/// actor, and any other agent is a plain (non-workflow) subagent, which is the
/// same derivation `run_native_key` uses for that shape.
fn canonical_todo_actor_run(
    output: &FactBatch,
    context: &ClaudeTodoContext,
) -> Result<crate::adapter::CanonicalEntityKey, AdapterError> {
    if context.native_agent_id == context.native_session_id {
        output.canonical_root_actor_run_key(context.native_session_id.as_bytes(), None)
    } else {
        let native_key = format!(
            "{}\0\0{}",
            context.native_session_id, context.native_agent_id
        );
        output.canonical_entity_key("run", native_key.as_bytes())
    }
}
```

Note `"{}\0\0{}"` — the doubled NUL is the empty workflow segment, matching `run_native_key`.

### 3.4 Field-by-field verdict: declared vs actually read

| Field | Where declared | Actually read? |
|---|---|---|
| `agentId` (from filename) | `project_parser.rs:860-868`; `adapter.rs:1071-1077` | **YES — primary child identity.** Path A: `subagents.agent_id` + the substring probe at `core/writer.rs:1143`. Path B: third segment of `run_native_key`, `ActorRunRevisionFact.native_actor_id`, `DelegationFact.native_child_id`, `DelegationMetadataFact.native_child_id`. |
| `agentId` (inside a record) | `UserMessage.agent_id` `session.rs:277-278`; `AssistantMessage.agent_id` `session.rs:361-362`; `ProgressMessage.agent_id` `session.rs:431-432`; `AgentProgress.agent_id` `session.rs:478-479` | **NO — deserialized, never read.** Round-trips through re-serialization into `subagent_messages.data`. |
| `agentId` (workflow journal) | `ClaudeWorkflowJournalDocument.agent_id` `sidecars.rs:214` | **YES — load-bearing.** Required non-empty (`sidecars.rs:1100-1107`); forms `child_run_native_key` (`sidecars.rs:1150-1153`) and `WorkflowMemberEventFact.native_agent_id` (`:1196`). Fixture `adapter.rs:5829`, assertion `adapter.rs:5847`. |
| `sourceToolUseID` | `UserMessage.source_tool_use_id`, `#[serde(rename = "sourceToolUseID")]` — `session.rs:271-276` | **NO.** Zero read sites in the crate (grep-verified). |
| `sourceToolAssistantUUID` | `UserMessage.source_tool_assistant_uuid`, `#[serde(rename = "sourceToolAssistantUUID")]` — `session.rs:265-270` | **NO.** Zero read sites. |
| `parentToolUseID` | `ProgressMessage.parent_tool_use_id`, `#[serde(default, rename = "parentToolUseID")]` — `session.rs:429-430` | **NO.** Zero read sites. |
| `parentUuid` | `BaseMessageFields.parent_uuid` — `session.rs:65-66`; loose read at `adapter.rs:1983` | **Partly.** Path B: becomes `MessageFact.parent_native_message_id` (`adapter.rs:2029`) — intra-transcript threading, **not** cross-agent linkage. Path A: only inside `raw_json`. |
| `workflowId` | dir name `project_parser.rs:678-680`; `adapter.rs:1078-1080` | **YES.** Path A: `SubagentTranscript.workflow_id`, part of the uniqueness key on both `subagents` and `subagent_messages` (`core/schema.rs:355`, `:372`). Path B: middle segment of `run_native_key`, and part of `message_native_key`. |
| `spawnDepth` | `SubagentMeta.spawn_depth: Option<f64>` `project.rs:103`; `ClaudeSubagentMetadataDocument.spawn_depth: Option<u32>` `sidecars.rs:24`; `DelegationMetadataFact.spawn_depth` `adapter/facts.rs:2063` | **Path A: NO** — parsed then discarded; `core/writer.rs:1296-1308` reads only `agent_type` and `worktree_path` from `meta`. **Path B: YES** — `adapter.rs:1772` → `spawn_depth` column (`core/schema.rs:1219`, `:1284`; `engine/projection.rs:2697`, `:2727`, `:2892`), surfaced in `engine/orchestration_query.rs:87`, `:571`, `:1019`, `:1811`; `engine/timeline_query.rs:1398`; `engine/search_query.rs:924`. |
| `worktreePath` | `SubagentMeta.worktree_path` `project.rs:105` | **YES both paths.** Path A → `subagents.worktree_path` (`core/writer.rs:1305-1308`, schema comment at `core/schema.rs:349-352`). Path B → `DelegationMetadataFact.worktree_path` (`adapter.rs:1773`). |
| `teamName` (sidecar) | `ClaudeSubagentMetadataDocument.team_name` `sidecars.rs:22` | **Path B only** → `emit_team_affiliation` (`adapter.rs:1777-1791`). Path A's `SubagentMeta` does not even declare it. |
| `isSidechain` | `session.rs:75-76`; `project.rs:55-56` | Only the `first_prompt` filter (`session_metadata.rs:60`); otherwise stored/projected. |

---

## 4. `SubagentType` values and where nested/workflow subagents differ

### The enum

`/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/project.rs:63-71`:

```rust
/// TS `SubagentType = 'task' | 'prompt_suggestion' | 'compact'`. Kept as a
/// string-backed enum so serde can deserialize the literal values directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentType {
    Task,
    PromptSuggestion,
    Compact,
}
```

Wire values: `"task"`, `"prompt_suggestion"`, `"compact"`.

### Inference is purely a filename substring test

`project_parser.rs:870-884`:

```rust
/// TS:
/// ```ts
/// if (fileName.includes('prompt_suggestion')) return 'prompt_suggestion';
/// if (fileName.includes('compact')) return 'compact';
/// return 'task';
/// ```
fn infer_agent_type(file_name: &str) -> SubagentType {
    if file_name.contains("prompt_suggestion") {
        SubagentType::PromptSuggestion
    } else if file_name.contains("compact") {
        SubagentType::Compact
    } else {
        SubagentType::Task
    }
}
```

Order matters (`prompt_suggestion` is tested first), it is `contains` — not a prefix or suffix test — and it runs on the **whole file name** including the `agent-` prefix and the `.jsonl` extension. So an agent whose id happens to contain the substring `compact` would be typed `Compact`.

### The enum is a fallback, not the stored value

`core/writer.rs:1292-1301` (re-quoted for this section):

```rust
// Prefer the sidecar's real agent type (general-purpose, Explore, …)
// over the filename-inferred enum kind. `to_string` on the enum
// produces `"task"` (with quotes) — strip them to store the bare
// string, matching the TS convention.
let agent_type = match transcript.meta.as_ref() {
    Some(meta) => meta.agent_type.clone(),
    None => serde_json::to_string(&transcript.agent_type)?
        .trim_matches('"')
        .to_string(),
};
```

So `subagents.agent_type` holds the sidecar's free-form `agentType` (`"Explore"`, `"general-purpose"`, …) whenever `agent-{id}.meta.json` exists and parses; the three-valued enum only surfaces when it does not. `SubagentTranscript.agent_type` is `SubagentType` (`project.rs:80`) but the column is `TEXT`.

Path B has no equivalent enum at all — it requires a non-empty `agentType` from the sidecar or preserves the record as unknown (`adapter.rs:1736-1744`). There is no filename-based type inference on Path B.

The full container — `claude/types/project.rs:73-90`:

```rust
/// TS `SubagentTranscript`. The parser never serialises this back out, but
/// we derive `Serialize` anyway so the writer can re-emit it as JSON when
/// storing the aggregate `messages` blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTranscript {
    pub agent_id: String,
    pub agent_type: SubagentType,
    pub file_name: String,
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<SubagentMeta>,
    /// The `wf_...` run this transcript belongs to, or `""` for a
    /// top-level (non-workflow) subagent.
    #[serde(default)]
    pub workflow_id: String,
}
```

### Where workflow/nested subagents differ

| Aspect | Flat subagent | Workflow-nested subagent |
|---|---|---|
| Location | `<sid>/subagents/agent-*.jsonl` | `<sid>/subagents/workflows/<wf>/agent-*.jsonl` |
| Path A filename filter | any `*.jsonl` (`project_parser.rs:661`) | must start `agent-` (`:688`) |
| Path A depth | flat only | one level only |
| `workflow_id` | `""` (`project_parser.rs:664`; `types/project.rs:86-89`) | directory name (`:691`) |
| `run_native_key` (Path B) | `sid \0 "" \0 agentId` | `sid \0 wf_x \0 agentId` |
| Sidecar `description` | usually present | **absent** — `types/project.rs:97`: *"Absent on nested workflow metas — must be optional or their parse fails."* |
| Extra evidence | none | `journal.jsonl` `started`/`result` events → `Fact::WorkflowMemberEvent` |
| Workflow id validation | n/a | Path A none; Path B requires `wf_` + non-empty suffix (`adapter.rs:1566-1574`) |
| Affiliation | none | `ActorAffiliationRevisionFact` with `ActorAffiliationDimension::Workflow` (`sidecars.rs:1202-1217`) |
| Uniqueness | `UNIQUE(source_id, project_slug, session_id, workflow_id, agent_id)` — `core/schema.rs:355` — the same `agent_id` can legitimately appear once flat and once per workflow |

`WorkflowRun` type header — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/types/workflow.rs:1-27`:

```rust
//! Workflow run record — TS `WorkflowRun` (types/index.ts).
//!
//! One agent-orchestration run at
//! `projects/{slug}/{sessionId}/workflows/{runId}.json`. Session-scoped;
//! its nested subagent transcripts group to it via
//! `SubagentTranscript.workflow_id`.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub workflow_id: String,
    pub name: String,
    pub status: String,
    pub agent_count: f64,
    pub total_tokens: f64,
    pub total_tool_calls: f64,
    pub duration_ms: f64,
    pub subagent_count: f64,
    /// The full raw `{runId}.json` run record.
    pub data: Value,
    /// Parsed `journal.jsonl` entries (started/result events).
    #[serde(default)]
    pub journal: Vec<Value>,
}
```

Journal document shape — `sidecars.rs:209-216`:

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeWorkflowJournalDocument {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) agent_id: String,
    pub(super) key: String,
}
```

Event-kind discrimination — `sidecars.rs:1116-1145`:

```rust
let (kind, result) = match document.kind.as_str() {
    "started" if value.get("result").is_none() => (WorkflowMemberEventKind::Started, None),
    "result" => {
        let Some(result) = value.get("result").cloned() else {
            return preserve_workflow_contract_loss(
                record, output, "workflow_journal",
                "result record is missing its result value",
            );
        };
        (WorkflowMemberEventKind::Result, Some(result))
    }
    "started" => {
        return preserve_workflow_contract_loss(
            record, output, "workflow_journal",
            "started record unexpectedly contains a result value",
        );
    }
    _ => {
        return preserve_workflow_contract_loss(
            record, output, "workflow_journal",
            "record has an unsupported event type",
        );
    }
};
```

Member key derivation — `sidecars.rs:1147-1166`:

```rust
let workflow = workflow_key(adapter_id, record.source_instance_id, context)?;
let mut member_native_key = workflow_native_key(context);
push_key_component(&mut member_native_key, native_agent_id.as_bytes());
let child_run_native_key = format!(
    "{}\0{}\0{}",
    context.native_session_id, context.native_workflow_id, native_agent_id
);
```

— note this reconstructs exactly the same `sid \0 wf \0 agent` shape as `run_native_key`, which is what makes a journal event and a nested transcript resolve to the same run.

Member reduction — `engine/workflow_projection.rs:472-490`:

```rust
let member_status = match (decisive_started, decisive_result) {
    (Some(_), Some(_)) => "result_observed",
    (Some(_), None) => "started",
    (None, Some(_)) => "orphan_result",
    (None, None) => {
        return Err(EngineError::InvalidCommit(
            "workflow member contains no supported event assertions".to_string(),
        ));
    }
};
let resolution_status = if competing_started_count > 0
    || competing_result_count > 0
    || event_key_conflict
    || identity_conflict
{
    "conflicting"
} else {
    "resolved"
};
```

Run-doc-vs-journal reconciliation — `engine/workflow_projection.rs:634-641`:

```rust
let membership_count_status = match (decisive_snapshot, members.observed_count) {
    (_, 0) => "unobserved",
    (None, _) => "snapshot_missing",
    (Some(snapshot), count) if snapshot.agent_count == sqlite_usize(count, "member count")? => {
        "matched"
    }
    (Some(_), _) => "different",
};
```

and the identity join check — `engine/workflow_projection.rs:607-621`:

```rust
let join_conflict = members.identity_conflict
    || decisive_snapshot.is_some_and(|snapshot| {
        members.session_key.as_deref().is_some_and(|session| session != snapshot.session_key)
            || members.project_key.as_deref().is_some_and(|project| project != snapshot.project_key)
            || members.native_workflow_id.as_deref()
                .is_some_and(|native_id| native_id != snapshot.native_workflow_id)
    });
```

Terminal workflow state is deliberately never propagated to child runs — `engine/workflow_projection.rs:1-7`:

```rust
//! Workflow run-summary and append-journal projection.
//!
//! Run documents are independently replaceable snapshots. Journal records
//! accumulate within one append generation and establish workflow membership
//! plus native started/result observations. Workflow terminal state is never
//! copied to a child run: completed workflows in the native corpus can retain
//! members without result records and can contain member-level errors.
```

---

## 5. How `sessions-index.json` is read and reconciled with actual JSONL

### Path A — read, tolerate, merge, re-serialize

**Read** — `project_parser.rs:487-509`:

```rust
/// Read + parse `sessions-index.json` from `<project_dir>/sessions-index.json`.
///
/// Returns the parsed [`SessionsIndex`]. On a missing or malformed file we
/// fall back to a synthetic empty index — matching TS `parseSessionsIndex`,
/// which reconstructs the index from disk rather than persisting the raw
/// (possibly malformed) file text.
fn read_sessions_index(project_dir: &Path) -> SessionsIndex {
    let path = project_dir.join("sessions-index.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return empty_sessions_index();
    };
    serde_json::from_str::<SessionsIndex>(&raw).unwrap_or_else(|_| empty_sessions_index())
}

fn empty_sessions_index() -> SessionsIndex {
    SessionsIndex {
        version: 1,
        original_path: None,
        entries: Vec::new(),
    }
}
```

Missing **and** malformed both degrade to the same synthetic empty index.

**Merge** — `project_parser.rs:511-535`:

```rust
/// Port of TS `mergeWithDiscoveredEntries` — appends any on-disk JSONL
/// files whose session IDs aren't already present in the index.
fn merge_with_discovered_entries(
    index_entries: Vec<SessionIndexEntry>,
    project_dir: &Path,
    original_path: Option<&str>,
) -> Result<Vec<SessionIndexEntry>, ParseError> {
    let mut indexed: std::collections::HashSet<String> =
        index_entries.iter().map(|e| e.session_id.clone()).collect();
    let mut merged = index_entries;

    // Sorted, because the source is a directory listing and neither engine
    // gets a guaranteed order from one. NTFS returns entries sorted while ext4
    // and APFS do not, so an unsorted merge agreed on Windows and disagreed on
    // Linux and macOS — a cross-engine divergence that depended on the
    // developer's filesystem (RFC 008 Phase 5).
    let mut discovered = discover_session_entries(project_dir, original_path)?;
    discovered.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    for entry in discovered {
        if indexed.insert(entry.session_id.clone()) {
            merged.push(entry);
        }
    }
    Ok(merged)
}
```

Index entries win and keep their original order; discovered-only entries are appended, sorted, after them. Nothing is ever *removed* for being index-only-with-no-file.

**Discover** — `project_parser.rs:537-636`. Directory-enumeration failure is **project-fatal**, not a skipped record:

```rust
/// Port of TS `discoverSessionEntries` — scans the project dir for
/// canonical `<uuid>.jsonl` files and builds a stub entry for each.
///
/// We skip the TS "peek at first user prompt" streaming read here: the
/// real parser reads the whole file immediately afterwards anyway, so
/// re-reading just to extract a 200-char prompt is wasteful. The entry's
/// `first_prompt` stays empty — the writer fills it in from the first
/// user message it ingests.
fn discover_session_entries(
    project_dir: &Path,
    original_path: Option<&str>,
) -> Result<Vec<SessionIndexEntry>, ParseError> {
    // Enumerating the project directory is what tells us which sessions exist.
    // If it fails we cannot know, and a merged index built from the
    // sessions-index alone would look complete while silently omitting every
    // session on disk — so this is fatal to the project rather than a skipped
    // record (RFC 008 Phase 2).
    //
    // A missing directory is not a failure: the slug came from a directory
    // listing, so this means it was deleted mid-run, which the next warm start
    // handles as the deletion it is.
    if let Some(err) = fault::injected(project_dir) {
        return Err(ParseError::Fatal {
            path: project_dir.to_string_lossy().into_owned(),
            message: err,
        });
    }
    let read_dir = match std::fs::read_dir(project_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(ParseError::Fatal {
                path: project_dir.to_string_lossy().into_owned(),
                message: e.to_string(),
            })
        }
    };
```

(The doc comment about skipping the prompt peek is now stale — the code below it does peek.)

Stub entry construction — `project_parser.rs:584-635`:

```rust
    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else { continue; };
        if !UUID_JSONL.is_match(name) { continue; }
        let path = entry.path();
        let session_id = name.trim_end_matches(".jsonl").to_owned();

        let file_mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            // Node computes `stats.mtimeMs` as `sec * 1000 + nsec / 1e6`. Computing
            // `as_secs_f64() * 1000.0` instead rounds differently in the last
            // millisecond, and both engines then truncate when rendering three
            // decimal digits — which turned a floating-point hair's breadth into
            // a visible 1 ms disagreement on session timestamps (RFC 008 Phase 5).
            .map(|d| d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1_000_000.0)
            .unwrap_or(0.0);

        // Port of the TS discoverSessionEntries: set `created` and
        // `modified` from file mtime as ISO-8601 (matches what
        // `new Date(mtimeMs).toISOString()` produces), and peek at the
        // file's first user message for `first_prompt`. Without these,
        // projects that have no sessions-index.json end up with blank
        // timestamps (UI sort-by-modified breaks) and all sessions
        // labeled "No prompt".
        let modified_iso = epoch_ms_to_iso8601(file_mtime);
        let first_prompt = peek_first_user_prompt(&path).unwrap_or_else(|| "No prompt".to_owned());

        out.push(SessionIndexEntry {
            session_id,
            full_path: path.to_string_lossy().into_owned(),
            file_mtime,
            first_prompt,
            // `Some("")`, not `None` — a discovered entry carries an empty
            // summary, which TS emits as `""`. `None` is reserved for the key
            // being absent from an index file.
            summary: Some(String::new()),
            message_count: 0,
            created: modified_iso.clone(),
            modified: modified_iso,
            git_branch: String::new(),
            project_path: project_path.clone(),
            is_sidechain: false,
        });
    }
```

The `summary: Option<String>` distinction is deliberate — `claude/types/project.rs:33-44`:

```rust
/// `Option` rather than `String` because TS distinguishes two states that
/// `#[serde(default)]` on a `String` collapses into one: the key **absent**
/// from the index entry (TS omits it and writes SQL `NULL`) and the key
/// present but empty (TS writes `''`). The real corpus has both — 6 NULL
/// and 117 empty — so defaulting to `String` made Rust write `''` for all
/// 123 and diverge on exactly the 6.
///
/// `skip_serializing_if` keeps the round-trip faithful: a `None` must be
/// omitted again, not re-emitted as `null`, or `projects.sessions_index`
/// would diverge instead.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub summary: Option<String>,
```

**Persist the MERGED index** — `project_parser.rs:151-209`, the whole body:

```rust
fn parse_project_body(
    &self,
    root_dir: &Path,
    slug: &str,
    events: &Sender<IngestEvent>,
) -> Result<(), ParseError> {
    let project_dir = root_dir.join("projects").join(slug);

    // 1. Read + parse sessions-index.json (or synthesise an empty one).
    let sessions_index = read_sessions_index(&project_dir);
    let original_path = sessions_index
        .original_path
        .clone()
        .unwrap_or_else(|| slug_to_path(slug));

    // 2. Merge index entries with any on-disk JSONL files the index doesn't
    //    already know about — matches TS `mergeWithDiscoveredEntries`.
    let merged_index = SessionsIndex {
        version: sessions_index.version,
        entries: merge_with_discovered_entries(
            sessions_index.entries,
            &project_dir,
            sessions_index.original_path.as_deref(),
        )?,
        original_path: sessions_index.original_path,
    };

    // 3. Emit the Project event storing the serialized MERGED index
    //    (parity with TS `onProject`, which stores
    //    `JSON.stringify(parseSessionsIndex(...))`). Storing the raw file
    //    text here would drop discovered-but-unindexed sessions from
    //    `projects.sessions_index`.
    let sessions_index_json =
        serde_json::to_string(&merged_index).unwrap_or_else(|_| "{}".to_owned());
    events.send(IngestEvent::Project {
        slug: slug.to_owned(),
        original_path,
        sessions_index_json,
    })?;

    // 4. MEMORY.md (optional)
    if let Some(memory) = read_project_memory(&project_dir) {
        events.send(IngestEvent::ProjectMemory {
            slug: slug.to_owned(),
            content: memory,
        })?;
    }

    // 5. Walk sessions. The only error returned from `parse_one_session`
    //    is `ChannelClosed` — propagate it immediately so we don't spin
    //    over a dead channel.
    for entry in &merged_index.entries {
        parse_one_session(root_dir, &project_dir, slug, entry, events)?;
    }

    // The terminal event is emitted by `parse_project`'s guard, not here —
    // see its doc comment.
    Ok(())
}
```

Test pinning the merge — `project_parser.rs:1735-1777`:

```rust
#[test]
fn no_sessions_index_stores_merged_discovered_entries() {
    // A project with a session JSONL but NO sessions-index.json must store
    // a MERGED index containing the discovered entry — not "{}" (which is
    // what storing the raw missing-file fallback would produce).
    ...
    assert_ne!(json, "{}", "merged index must not be the empty fallback");
    ...
    assert_eq!(entries.len(), 1, "discovered session must be in the index");
    // The discovered entry carries the peeked first prompt, not blank.
    assert_eq!(entries[0].get("firstPrompt").and_then(|v| v.as_str()), Some("hi m1"));
}
```

**Per-session file resolution** — `project_parser.rs:275-299`:

```rust
fn parse_one_session(
    root_dir: &Path,
    project_dir: &Path,
    slug: &str,
    entry: &SessionIndexEntry,
    events: &Sender<IngestEvent>,
) -> Result<(), ParseError> {
    let session_id = entry.session_id.clone();

    events.send(IngestEvent::Session {
        slug: slug.to_owned(),
        entry: entry.clone(),
    })?;

    // Canonical path, with fallback to entry.full_path if the canonical
    // file doesn't exist (handles stale indices pointing at relocated
    // JSONL files). Port of the TS `filePath` ternary.
    let canonical_path = project_dir.join(format!("{session_id}.jsonl"));
    let file_path: PathBuf = if canonical_path.exists() {
        canonical_path.clone()
    } else if !entry.full_path.is_empty() && Path::new(&entry.full_path).exists() {
        PathBuf::from(&entry.full_path)
    } else {
        canonical_path.clone()
    };
```

An index entry with no file on disk still emits `IngestEvent::Session` + `SessionComplete` with `message_count = 0` — sessions are never dropped for lack of a transcript. It also still gets its subagents, workflows, tool-results, file-history, todos, and task read (`project_parser.rs:381-447`).

The first-prompt peek — `project_parser.rs:1194-1215`:

```rust
/// Read the first genuine human prompt from a JSONL session file.
///
/// Claude uses user-role envelopes for local commands, metadata, compact
/// summaries, and tool results. Those are skipped until a human text record is
/// found. Text is truncated to 200 UTF-16 code units for TS parity.
fn peek_first_user_prompt(path: &Path) -> Option<String> {
    use std::cell::RefCell;

    let found: RefCell<Option<String>> = RefCell::new(None);
    let _ = crate::core::jsonl::read_jsonl_streaming(path, 0, |line, _, _| {
        if found.borrow().is_some() { return; }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else { return; };
        if let Some(prompt) = session_metadata::extract_human_prompt(&val) {
            *found.borrow_mut() = Some(prompt);
        }
    });
    found.into_inner()
}
```

And the writer normalizes synthetic prompts on ingest — `core/writer.rs:1198-1204`:

```rust
IngestEvent::Session { slug, entry } => {
    let now = now_ms();
    let first_prompt = if source_id == "claude-code" {
        crate::claude::session_metadata::normalize_first_prompt(&entry.first_prompt)
    } else {
        entry.first_prompt.clone()
    };
```
with the synthetic-prefix list at `session_metadata.rs:12-21`:
```rust
const SYNTHETIC_USER_PREFIXES: &[&str] = &[
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<command-name>",
    "<command-message>",
    "<task-notification>",
    "<system-reminder>",
    "<ide_opened_file>",
    "<ide_selection>",
];
```

`original_path`: `sessions_index.original_path` if present, else `slug_to_path(slug)` (`project_parser.rs:161-164`); the same fallback is applied per-entry inside discovery (`:575-582`):

```rust
let slug_fallback = project_dir
    .file_name()
    .and_then(|s| s.to_str())
    .map(slug_to_path);
let project_path = original_path
    .map(str::to_owned)
    .or(slug_fallback)
    .unwrap_or_default();
```

### Path B — index is Supplemental, never authoritative

Stream declaration at `adapter.rs:556-574`: `StreamAuthority::Supplemental`, `EntityScope::Project`, `ConsistencyPolicy::SnapshotReplace`, cap 1 MiB (`SESSION_INDEX_MAX_BYTES`, `adapter.rs:165`).

Document shape — `sidecars.rs:218-242`:

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeSessionIndexDocument {
    pub(super) version: u64,
    #[serde(default)]
    pub(super) original_path: Option<String>,
    pub(super) entries: Vec<ClaudeSessionIndexEntryDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeSessionIndexEntryDocument {
    pub(super) session_id: String,
    pub(super) full_path: String,
    pub(super) file_mtime: u64,
    pub(super) first_prompt: String,
    #[serde(default)]
    pub(super) summary: Option<String>,
    pub(super) message_count: u64,
    pub(super) created: String,
    pub(super) modified: String,
    pub(super) git_branch: String,
    pub(super) project_path: String,
    pub(super) is_sidechain: bool,
}
```

Note this is **strict** where Path A is lenient: `full_path`, `file_mtime`, `first_prompt`, `message_count`, `created`, `modified`, `git_branch`, `project_path`, `is_sidechain` are all required (no `#[serde(default)]`), whereas `claude/types/project.rs:25-57` defaults every one of them.

Decoding — `sidecars.rs:1221-1343`, the validation block:

```rust
    if document.version != 1 {
        return preserve_session_index_contract_loss(
            record, output, "has an unsupported native version",
        );
    }
    if document.entries.len() > SESSION_INDEX_ENTRY_LIMIT {
        return preserve_session_index_contract_loss(
            record, output,
            &format!("exceeds the {SESSION_INDEX_ENTRY_LIMIT} entry bound"),
        );
    }
    if document.original_path.as_deref().is_some_and(|path| path.trim().is_empty()) {
        return preserve_session_index_contract_loss(record, output, "has an empty originalPath");
    }

    let mut session_ids = BTreeSet::new();
    let mut entries = Vec::with_capacity(document.entries.len());
    for entry in document.entries {
        if !is_uuid(&entry.session_id) {
            return preserve_session_index_contract_loss(
                record, output, "contains a non-UUID sessionId",
            );
        }
        if !session_ids.insert(entry.session_id.clone()) {
            return preserve_session_index_contract_loss(
                record, output, "contains duplicate sessionId entries",
            );
        }
        for (field, value) in [
            ("fullPath", entry.full_path.as_str()),
            ("firstPrompt", entry.first_prompt.as_str()),
            ("created", entry.created.as_str()),
            ("modified", entry.modified.as_str()),
            ("projectPath", entry.project_path.as_str()),
        ] {
            if value.trim().is_empty() {
                return preserve_session_index_contract_loss(
                    record, output,
                    &format!("contains an entry with empty {field}"),
                );
            }
        }
        entries.push(SessionIndexEntrySnapshot {
            session: EntityKey::native(
                adapter_id, record.source_instance_id, "session", entry.session_id.as_bytes(),
            )?,
            native_session_id: entry.session_id,
            full_path: entry.full_path,
            file_mtime_ms: entry.file_mtime,
            first_prompt: entry.first_prompt,
            summary: entry.summary,
            message_count: entry.message_count,
            created_at: native_timestamp(&entry.created),
            modified_at: native_timestamp(&entry.modified),
            git_branch: entry.git_branch,
            project_path: entry.project_path,
            is_sidechain: entry.is_sidechain,
        });
    }

    output.push(
        record,
        Fact::SessionIndexSnapshot(SessionIndexSnapshotFact {
            project: EntityKey::native(
                adapter_id, record.source_instance_id, "project",
                context.project_slug.as_bytes(),
            )?,
            native_project_key: context.project_slug.clone(),
            native_version: document.version,
            original_path: document.original_path,
            entries,
            native_snapshot,
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}
```

**Any** of those failures preserves the *whole document* as a contract loss rather than partially applying it. `SESSION_INDEX_ENTRY_LIMIT` = 4096 (`adapter.rs:173`).

The projection is where reconciliation with actual transcripts happens — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/engine/session_index_projection.rs:1-7`:

```rust
//! Replaceable Claude project session-index projection.
//!
//! A native index is useful metadata, but it is not transcript evidence. Index
//! entries therefore remain queryable when their JSONL is absent and join to
//! canonical transcript sessions only when an independently committed
//! `SessionFact` exists. Complete project snapshots compete deterministically;
//! child metadata and cross-project identity disagreement remain explicit.
```

The join — `session_index_projection.rs:440-457`:

```rust
let transcript_project = transaction
    .query_row(
        "SELECT project_key FROM canonical_sessions WHERE session_key = ?1",
        [session_key],
        |row| row.get::<_, Vec<u8>>(0),
    )
    .optional()
    .map_err(|error| sqlite_error("join session index entry to transcript", error))?;
let (transcript_status, join_conflict) = match transcript_project {
    Some(project_key) if project_key != *decisive_project_key => ("different_project", true),
    Some(_) => ("present", false),
    None => ("missing", false),
};
let resolution_status = if competing_entry_count > 0 || identity_conflict || join_conflict {
    "conflicting"
} else {
    "resolved"
};
```

So each `canonical_session_index_entries` row carries `transcript_status ∈ {present, missing, different_project}` and `resolution_status ∈ {resolved, conflicting}` plus `assertion_count`, `competing_entry_count`, `identity_conflict`, `join_conflict` (written at `:458-514`).

Project-level snapshot competition — `session_index_projection.rs:293-311`:

```rust
let assertion_count = assertions.len();
let competing_snapshot_count = assertions
    .iter()
    .map(|(_, digest)| digest)
    .collect::<BTreeSet<_>>()
    .len()
    .saturating_sub(1);
let status = if competing_snapshot_count > 0 { "conflicting" } else { "resolved" };
let entry_count = transaction
    .query_row(
        "SELECT COUNT(*) FROM session_index_entry_assertions WHERE fact_id = ?1",
        [decisive_fact_id],
        |row| row.get::<_, i64>(0),
    )
    .map_err(|error| sqlite_error("count decisive session index entries", error))?;
```

Entry competition includes a subtle "missing from some snapshots" term — `session_index_projection.rs:434-439`:

```rust
let competing_entry_count = assertions
    .iter()
    .collect::<BTreeSet<_>>()
    .len()
    .saturating_sub(1)
    .saturating_add(usize::from(assertion_count < project_snapshot_count));
```

Snapshot ownership guard — `session_index_projection.rs:82-92`:

```rust
// A transcript commit may still change a session-index join, but it does
// not own the index document. Only replace assertions when this object
// supplied (or previously supplied) the replace-document snapshot.
if owns_snapshot {
    transaction
        .execute(
            "DELETE FROM session_index_snapshot_assertions WHERE source_object_id = ?1",
            [object_id],
        )
        .map_err(|error| sqlite_error("replace session index snapshot", error))?;
}
```

Duplicate-session guard on write — `session_index_projection.rs:195-204`:

```rust
let mut session_keys = BTreeSet::new();
for (ordinal, entry) in fact.entries.iter().enumerate() {
    if !session_keys.insert(entry.session.as_bytes().to_vec()) {
        return Err(EngineError::InvalidCommit(
            "session index snapshot contains duplicate session keys".to_string(),
        ));
    }
    write_entry(transaction, envelope, fact, entry, ordinal)?;
}
```

Change topics emitted — `session_index_projection.rs:531`, `:551`, `:566`, `:589`: `history.session-index.changed`, `diagnostic.history.session-index-conflict`, `history.session-index-entry.changed`, `diagnostic.history.session-index-entry-conflict`.

### Path B — catalog discovery, a third independent read

`/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/claude/catalog_discovery.rs:1-25`, the whole header:

```rust
//! Claude Code catalog discovery.
//!
//! Claude spells catalog membership on the filesystem, so a complete catalog
//! needs no transcript bytes at all:
//!
//! ```text
//! <projects>/<project-slug>/                     -> a project
//! <projects>/<project-slug>/<uuid>.jsonl         -> a transcript-backed session
//! <projects>/<project-slug>/<uuid>/              -> a session with side data
//!                                                   (subagents, workflows, tool
//!                                                   results) and possibly no
//!                                                   transcript of its own
//! <projects>/<project-slug>/sessions-index.json  -> sessions the agent knows about,
//!                                                   including ones with no transcript
//! ```
//!
//! A third of the projects on a real machine are discoverable only through
//! that session-directory form, so ignoring it would silently lose them.
//!
//! `<project-slug>` is byte-identical to the `native_project_key` the Claude
//! decoder derives from the same path, so a discovered row and its later
//! transcript-backed history share one identity.
//!
//! Nested `subagents/agent-*.jsonl` transcripts belong to their parent session
//! and are deliberately not catalog members.
```

Scan is depth-pinned — `catalog_discovery.rs:68-77`:

```rust
let snapshot = DirectorySnapshot::new(DirectorySnapshotConfig {
    max_entries: limits.max_entries,
    max_entries_per_directory: limits.max_entries,
    // Claude's projects root is exactly two levels deep
    // (<projects>/<project>/<session>.jsonl), so the scan depth is
    // pinned rather than taken from `limits`.
    max_depth: 2,
})
```

Selection — `catalog_discovery.rs:246-270`:

```rust
/// Depth-2 selection: project directories and, inside each, the transcripts,
/// session directories, and session index. Session directories are recorded
/// but never descended into, so a deep `subagents/` or `workflows/` tree
/// costs nothing.
fn catalog_selection(path: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    let Ok(portable_path) = portable_relative_path(path) else {
        return DirectorySelection::Ignore;
    };
    let components = path_components(&portable_path);
    match (kind, components.len()) {
        (DirectoryEntryKind::Directory, 1) => DirectorySelection::Recurse,
        (DirectoryEntryKind::Directory, 2) if is_uuid(&components[1]) => {
            DirectorySelection::Include
        }
        (DirectoryEntryKind::File, 2) => {
            let name = &components[1];
            if name == SESSION_INDEX_NAME || name.ends_with(".jsonl") {
                DirectorySelection::Include
            } else {
                DirectorySelection::Ignore
            }
        }
        _ => DirectorySelection::Ignore,
    }
}
```

Three evidence classes with a precedence order. Session directory — `catalog_discovery.rs:120-132`:

```rust
// A session directory names a session whose transcript may or may not
// exist beside it. Discoverability and transcript availability are
// different facts, so it is admitted either way.
if entry.kind == DirectoryEntryKind::Directory {
    if !is_uuid(name) { continue; }
    insert_session(
        &mut sessions, &mut conflicts,
        session_row(slug, name, &entry.display_path, None),
    );
    continue;
}
```

Row builder — `catalog_discovery.rs:221-244`:

```rust
/// One transcript-backed or directory-only session row.
fn session_row(
    slug: &str, session_id: &str, provenance: &str, transcript: Option<(u64, i64)>,
) -> DiscoveredSession {
    DiscoveredSession {
        native_session_key: session_id.to_string(),
        native_session_id: Some(session_id.to_string()),
        native_project_key: slug.to_string(),
        association_basis: ProjectAssociationBasis::SessionDirectory,
        association_quality: AssociationQuality::Exact,
        association_provenance: provenance.to_string(),
        title: None,
        native_created_at: None,
        native_updated_at: None,
        native_message_count: None,
        transcript_locator: transcript.map(|_| provenance.to_string()),
        source_size_bytes: transcript.map(|(size, _)| size),
        source_modified_ms: transcript.map(|(_, modified)| modified),
        transcript_present: transcript.is_some(),
    }
}
```

Index merge — `catalog_discovery.rs:272-317`:

```rust
/// Index evidence refines a transcript-backed session and *introduces* one
/// that has no transcript. It never overwrites the directory association,
/// which is exact.
fn merge_index_entry(
    sessions: &mut BTreeMap<String, DiscoveredSession>,
    conflicts: &mut Vec<DiscoveredAssociationConflict>,
    slug: &str,
    provenance: &str,
    entry: IndexEntry,
) {
    if let Some(existing) = sessions.get_mut(&entry.session_id) {
        existing.title = existing.title.take().or(entry.title);
        existing.native_created_at = existing.native_created_at.take().or(entry.created);
        existing.native_updated_at = existing.native_updated_at.take().or(entry.modified);
        existing.native_message_count = existing.native_message_count.or(entry.message_count);
        if existing.native_project_key != slug {
            conflicts.push(DiscoveredAssociationConflict {
                native_session_key: entry.session_id,
                competing_native_project_key: slug.to_string(),
                basis: ProjectAssociationBasis::NativeProjectIndex,
                provenance: provenance.to_string(),
            });
        }
        return;
    }
    insert_session(
        sessions, conflicts,
        DiscoveredSession {
            native_session_key: entry.session_id.clone(),
            native_session_id: Some(entry.session_id),
            native_project_key: slug.to_string(),
            association_basis: ProjectAssociationBasis::NativeProjectIndex,
            association_quality: AssociationQuality::NativeClaimed,
            association_provenance: provenance.to_string(),
            title: entry.title,
            native_created_at: entry.created,
            native_updated_at: entry.modified,
            native_message_count: entry.message_count,
            transcript_locator: None,
            source_size_bytes: None,
            source_modified_ms: None,
            transcript_present: false,
        },
    );
}
```

Conflict resolution — `catalog_discovery.rs:319-364`:

```rust
/// Insert a session, keeping the higher-quality association and recording the
/// loser as an explicit conflict when the projects disagree.
///
/// A session commonly appears twice inside one project — once as its side-data
/// directory and once as its transcript file — so agreeing evidence merges
/// rather than racing: whichever arrives second still contributes its
/// transcript.
fn insert_session(
    sessions: &mut BTreeMap<String, DiscoveredSession>,
    conflicts: &mut Vec<DiscoveredAssociationConflict>,
    candidate: DiscoveredSession,
) {
    match sessions.get_mut(&candidate.native_session_key) {
        None => { sessions.insert(candidate.native_session_key.clone(), candidate); }
        Some(existing) => {
            if existing.native_project_key == candidate.native_project_key {
                if candidate.transcript_present && !existing.transcript_present {
                    existing.transcript_present = true;
                    existing.transcript_locator = candidate.transcript_locator;
                    existing.source_size_bytes = candidate.source_size_bytes;
                    existing.source_modified_ms = candidate.source_modified_ms;
                    existing.association_provenance = candidate.association_provenance;
                }
                return;
            }
            if candidate.association_quality > existing.association_quality {
                conflicts.push(DiscoveredAssociationConflict { ... existing ... });
                *existing = candidate;
            } else {
                conflicts.push(DiscoveredAssociationConflict { ... candidate ... });
            }
        }
    }
}
```

Index read here is lenient — a bad index only sets `degraded_reason` (`catalog_discovery.rs:178-184`, `:366-403`):

```rust
fn read_index_document(
    root: &Path, relative_path: &str, max_bytes: u64,
) -> Result<Vec<IndexEntry>, String> {
    let bounded = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let read = read_stable_file_confined(root, Path::new(relative_path), bounded)
        .map_err(|_| "claude session index could not be read".to_string())?;
    let bytes = match read {
        StableRead::Stable { bytes, .. } => bytes,
        StableRead::Missing => return Ok(Vec::new()),
        StableRead::Oversized(_) => {
            return Err("claude session index exceeds the catalog document bound".to_string())
        }
        StableRead::Unstable => {
            return Err("claude session index changed during the catalog pass".to_string())
        }
    };
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "claude session index is not valid JSON".to_string())?;
    let Some(entries) = document.get("entries").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let session_id = text(entry.get("sessionId")).filter(|id| is_uuid(id))?;
            Some(IndexEntry {
                session_id,
                title: text(entry.get("summary")).or_else(|| text(entry.get("firstPrompt"))),
                created: text(entry.get("created")),
                modified: text(entry.get("modified")),
                message_count: entry.get("messageCount").and_then(Value::as_u64),
                project_path: text(entry.get("projectPath")),
            })
        })
        .collect())
}
```

Note the title precedence: `summary` first, `firstPrompt` as fallback (`:395`).

---

## 6. Project-slug ↔ cwd path encoding/decoding

There is **no encoder** in this crate (encoding is Claude Code's own job); there is one decoder, and one deliberate refusal to decode.

### The decoder: `slug_to_path` — `project_parser.rs:1153-1192`

Two stages. First `slug_shape` classifies the platform — `project_parser.rs:1070-1131`:

```rust
/// The fixed leading portion of a decoded slug, plus the separator that
/// joins its segments and the still-encoded remainder.
///
/// Claude Code derives a slug from the project's absolute cwd, so the
/// leading characters tell us which platform wrote it:
///
/// | cwd                      | slug                     | shape         |
/// |--------------------------|--------------------------|---------------|
/// | `/Users/me/app`          | `-Users-me-app`          | `/`, `/`      |
/// | `D:\Projects\app`        | `D--Projects-app`        | `D:\`, `\`    |
/// | `D:\Projects\app` (old)  | `D:-Projects-app`        | `D:\`, `\`    |
///
/// A POSIX cwd is always absolute, so it always yields a leading `-`.
/// That makes "no leading dash, but starts with `<letter>--` or
/// `<letter>:-`" an unambiguous Windows drive marker. The two Windows
/// spellings both occur in the wild: current Claude Code folds the
/// colon into `-` (giving the doubled dash), older builds and the
/// Codex/Grok readers preserve it.
///
/// Detection is platform-independent on purpose — a synced `~/.claude`
/// must decode to the same path text on any host, and the CLI compares
/// these against `process.cwd()` verbatim.
struct SlugShape<'a> {
    /// Precedes the first segment: `/` on POSIX, `D:\` on Windows.
    prefix: String,
    /// Joins segments: `/` on POSIX, `\` on Windows.
    sep: char,
    /// The dash-encoded remainder, still to be split.
    rest: &'a str,
}

fn slug_shape(slug: &str) -> SlugShape<'_> {
    let bytes = slug.as_bytes();
    // `X--…` or `X:-…` — a Windows drive letter. Length 3 is the bare
    // drive root (`D--` → `D:\`), so `>=` not `>`.
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && (bytes[1] == b':' || bytes[1] == b'-')
        && bytes[2] == b'-'
    {
        let drive = (bytes[0] as char).to_ascii_uppercase();
        return SlugShape {
            prefix: format!("{drive}:\\"),
            sep: '\\',
            rest: &slug[3..],
        };
    }
    if let Some(rest) = slug.strip_prefix('-') {
        return SlugShape {
            prefix: "/".to_string(),
            sep: '/',
            rest,
        };
    }
    SlugShape {
        prefix: String::new(),
        sep: '/',
        rest: slug,
    }
}
```

Order matters: the drive test runs first, but a leading `-` can never reach it (`-` is not `is_ascii_alphabetic`), so POSIX always wins for absolute slugs — pinned by `project_parser.rs:1959-1965`:

```rust
/// A leading dash always wins: a POSIX directory that merely *starts*
/// with something drive-shaped must not be read as a Windows path.
#[test]
fn slug_to_path_prefers_posix_when_leading_dash_present() {
    assert_eq!(slug_to_path("-d--Projects-app"), "/d//Projects/app");
    assert_eq!(slug_to_path("-home-d--foo"), "/home/d//foo");
}
```

Second stage is a **greedy longest-match-first filesystem probe** — `project_parser.rs:1133-1192`:

```rust
/// Reconstruct the absolute path a project slug was derived from.
///
/// Claude Code encodes project paths into slugs by replacing every path
/// separator with `-`. Reversing that naively (every `-` → separator)
/// corrupts legitimate hyphens in directory names — e.g. a slug like
/// `-Users-me-Projects-vibe-ctl` can decode to either
/// `/Users/me/Projects/vibe/ctl` or `/Users/me/Projects/vibe-ctl`
/// depending on what actually exists on disk.
///
/// We resolve the ambiguity the same way the TS parser does: probe the
/// filesystem from left to right. At each position, try the longest
/// remaining suffix first (`vibe-ctl`, then `vibe-ctl-specs`, etc.). If
/// stat() succeeds, consume that range as a single segment and move on.
/// Fall back to the next-longest, and finally to a single segment.
///
/// For slugs whose underlying project dir no longer exists (stale
/// state, deleted project, or a Windows slug being decoded on a POSIX
/// host), the probe fails for every candidate and we degrade gracefully
/// to the naive one-segment-per-dash mapping — the display path may be
/// wrong but ingest still works.
fn slug_to_path(slug: &str) -> String {
    let SlugShape { prefix, sep, rest } = slug_shape(slug);
    if rest.is_empty() {
        return prefix;
    }

    let parts: Vec<&str> = rest.split('-').collect();
    let mut resolved = String::new();

    let mut i = 0;
    while i < parts.len() {
        let mut matched = false;
        for end in (i + 1..=parts.len()).rev() {
            let candidate_segment = parts[i..end].join("-");
            let probe_path = if resolved.is_empty() {
                format!("{prefix}{candidate_segment}")
            } else {
                format!("{prefix}{resolved}{sep}{candidate_segment}")
            };
            if std::fs::metadata(&probe_path).is_ok() {
                if !resolved.is_empty() {
                    resolved.push(sep);
                }
                resolved.push_str(&candidate_segment);
                i = end;
                matched = true;
                break;
            }
        }
        if !matched {
            if !resolved.is_empty() {
                resolved.push(sep);
            }
            resolved.push_str(parts[i]);
            i += 1;
        }
    }

    format!("{prefix}{resolved}")
}
```

**The probe is greedy per-position with no backtracking** — once a segment is accepted at position `i`, `i` jumps to `end` and that decision is never revisited. A wrong-but-existing longest match therefore strands the remainder into the naive fallback.

Callers: `project_parser.rs:164` (project `original_path` when the index has none) and `:578` (per-entry `project_path` fallback, derived from `project_dir.file_name()`).

Tests — `project_parser.rs:1925-1984`:

```rust
/// These assert the *naive* decode (no probe hits), which is what
/// runs whenever the encoded directory no longer exists on this host
/// — including every Windows slug decoded on a POSIX CI runner. The
/// probing behaviour is covered separately below.
#[test]
fn slug_to_path_decodes_posix_slugs() {
    assert_eq!(slug_to_path("-Users-me-Projects-app"), "/Users/me/Projects/app");
    assert_eq!(slug_to_path("-"), "/");
    assert_eq!(slug_to_path(""), "");
}

#[test]
fn slug_to_path_decodes_windows_drive_slugs() {
    // Current Claude Code folds the colon into a dash, so `D:\` → `D--`.
    assert_eq!(slug_to_path("D--Projects-p100-spaghetti"), "D:\\Projects\\p100\\spaghetti");
    assert_eq!(slug_to_path("C--Users-me"), "C:\\Users\\me");
    // Older builds (and the Codex/Grok readers) keep the colon.
    assert_eq!(slug_to_path("D:-I3T-WordplayAR"), "D:\\I3T\\WordplayAR");
    // Bare drive root.
    assert_eq!(slug_to_path("D--"), "D:\\");
    assert_eq!(slug_to_path("D:-"), "D:\\");
    // Drive letters normalize to uppercase so the two spellings of the
    // same project agree.
    assert_eq!(slug_to_path("d--Projects-app"), "D:\\Projects\\app");
}

/// The filesystem probe is what disambiguates hyphens that belong to a
/// directory name from hyphens that encode a separator. Build a real
/// tree and confirm the longest-match-first probe consumes `p100-app`
/// as one segment rather than splitting it.
#[test]
fn slug_to_path_probes_filesystem_to_keep_hyphenated_dirs() {
    let dir = mk_tempdir();
    let nested = dir.path().join("p100-app").join("sub");
    fs::create_dir_all(&nested).unwrap();

    // Re-encode the real temp dir the way Claude Code would, then
    // decode it back and expect the original path.
    let root = dir.path().to_str().unwrap();
    let slug = format!("{root}/p100-app/sub").replace(['/', '\\'], "-");
    // On Windows the temp root carries a drive letter, so re-encoding
    // yields the `C:-…` spelling; both shapes are handled.
    assert_eq!(slug_to_path(&slug), nested.to_str().unwrap());
}
```

### The refusal — catalog path reads `cwd` from a transcript head

`catalog_discovery.rs:186-200`:

```rust
// Current Claude installations commonly omit sessions-index.json. The
// slug cannot be reversed safely because '-' may be either a path
// separator or part of a directory name. Use the catalog contract's one
// bounded record-head read instead: it stays confined to the authorized
// projects root and gives the same cwd evidence the full decoder will
// later commit. Index evidence wins when it exists.
for (slug, candidate) in transcript_heads {
    let Some(project) = projects.get_mut(&slug) else {
        continue;
    };
    if project.display_path.is_none() {
        project.display_path =
            read_transcript_cwd(root, &candidate.relative_path, limits.max_head_bytes);
    }
}
```

The head read — `catalog_discovery.rs:210-219`:

```rust
/// Read complete JSONL records from one bounded prefix until Claude declares
/// its cwd. The final split may be a truncated record and is ignored.
fn read_transcript_cwd(root: &Path, relative_path: &str, max_head_bytes: u64) -> Option<String> {
    let bounded = usize::try_from(max_head_bytes).unwrap_or(usize::MAX);
    let bytes = read_prefix_confined(root, Path::new(relative_path), bounded).ok()??;
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .find_map(|record| text(record.get("cwd")))
}
```

Candidate selection prefers the newest transcript — `catalog_discovery.rs:56-62` and `:141-155`:

```rust
/// The one bounded transcript head selected to name a project when Claude has
/// no session index. Prefer the newest object so a moved workspace converges
/// on its current cwd without reading every transcript in the directory.
struct TranscriptHeadCandidate {
    relative_path: String,
    modified_ns: i128,
}
```
```rust
let candidate = TranscriptHeadCandidate {
    relative_path: entry.display_path.clone(),
    modified_ns: entry.modified_ns,
};
transcript_heads
    .entry(slug.clone())
    .and_modify(|current| {
        if (candidate.modified_ns, candidate.relative_path.as_str())
            > (current.modified_ns, current.relative_path.as_str())
        {
            current.relative_path.clone_from(&candidate.relative_path);
            current.modified_ns = candidate.modified_ns;
        }
    })
    .or_insert(candidate);
```

Precedence for `DiscoveredProject.display_path`: `sessions-index.json` `entries[].projectPath` first (`catalog_discovery.rs:168-184`), else the newest transcript's `cwd`, else `None`. The slug is never reversed.

The adapter/fact path likewise never decodes — `native_project_key` **is** the slug verbatim (`adapter.rs:1878`; `catalog_discovery.rs:113-117`; and the header claim at `catalog_discovery.rs:20-22`). Path B gets cwd from the record itself — `adapter.rs:1851-1858`:

```rust
let cwd = nonempty_field(&value, "cwd");
let git_branch = nonempty_field(&value, "gitBranch");
let cwd_changed =
    ClaudeTranscriptDecoderState::observe_field(&mut state.cwd_hash, cwd.as_deref());
let git_branch_changed = ClaudeTranscriptDecoderState::observe_field(
    &mut state.git_branch_hash,
    git_branch.as_deref(),
);
```

with change detection so a moved workspace re-declares its session (`adapter.rs:1864`), and the value landing in `SessionFact.cwd` (`:1879`).

Project enumeration for Path A is a plain sorted directory listing — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/orchestrate/ingest.rs:1043-1064`:

```rust
/// List immediate subdirectories of `<agent_dir>/projects/`. Each dir
/// name is a project slug. Non-directory entries (e.g. `.DS_Store`) are
/// skipped silently.
fn scan_project_slugs(agent_dir: &Path) -> std::result::Result<Vec<String>, std::io::Error> {
    let projects_dir = agent_dir.join("projects");
    if !projects_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut slugs: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&projects_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            slugs.push(name.to_owned());
        }
    }
    slugs.sort();
    Ok(slugs)
}
```

Easy to confuse but unrelated: `platform_path_key` — `/Users/jamesyong/Projects/project100/p008/spaghetti/crates/spaghetti-napi/src/source/file.rs:465-473` — is a length-prefixed byte encoding of a path used as a *source-instance identity key*, not a slug codec:

```rust
pub fn platform_path_key(path: &Path) -> Vec<u8> {
    let mut output = vec![1];
    for component in path.components() {
        let bytes = os_bytes(component.as_os_str());
        output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        output.extend_from_slice(&bytes);
    }
    output
}
```

---

## 7. Divergences and sharp edges

1. **`^agent-(a.+)\.jsonl$` vs `^agent-(.+)\.jsonl$` vs `strip_prefix("agent-")`.** `project_parser.rs:50` requires the agent id to begin with `a`; `fingerprint.rs:94` and `adapter.rs:1072` do not. An id not starting with `a` gets `agent_id = "agent-<id>"` in Path A and `"<id>"` in Path B. The Path A value then fails the substring test at `core/writer.rs:1143`, silently downgrading `link_method` to `"unlinked"` — the exact failure mode the RFC 008 Phase 5 comment says cost 113 of 638 subagents their linkage.

2. **Path A's nested workflow walk is one level deep** (`project_parser.rs:682-694`) while the declared glob and Path B allow `*/*/subagents/workflows/*/**/agent-*.jsonl` (`adapter.rs:354`; fixture `adapter.rs:4058` uses `.../wf_main/agents/agent-child.jsonl`). Path A misses those transcripts entirely; the fingerprint walk (`fingerprint.rs:628-654`) also only descends one level.

3. **Path A's flat walk accepts any `*.jsonl`** under `subagents/` (`project_parser.rs:661`), so a stray non-`agent-` file becomes a `SubagentTranscript` whose `agent_id` is the bare stem and whose `agent_type` is `Task`.

4. **Subagent messages are re-serialized, not stored verbatim** (`core/writer.rs:1348`, `:1368`) — unmodelled JSON keys are lost except where a `#[serde(flatten)] extra` map catches them (`session.rs:400-403`, `:213-214`).

5. **`sourceToolUseID`, `sourceToolAssistantUUID`, `parentToolUseID` are dead weight** in this crate — declared for shape fidelity, read nowhere. The parent link is filename-`agentId` + `tool_result` substring (Path A) or directory layout + `meta.json` `toolUseId` (Path B). Anyone expecting these to be the join key will be wrong.

6. **`spawnDepth` is parsed but discarded on Path A** — `SubagentMeta.spawn_depth` (`types/project.rs:103`) is never read by `core/writer.rs` (`:1296-1308` pulls only `agent_type` and `worktree_path`); only Path B's `DelegationMetadataFact` persists it.

7. **`workflow_id` is unvalidated on Path A** (any directory name under `workflows/`), strictly `wf_`-prefixed on Path B (`adapter.rs:1566-1574`); and Path A takes the run id from the body's `runId` (`project_parser.rs:786-790`) while Path B *rejects* a `runId`/filename mismatch (`sidecars.rs:994-1001`).

8. **`isSidechain` does no grouping anywhere in this crate.** Its only behavioural effect is disqualifying a line from becoming `first_prompt` (`session_metadata.rs:60`).

9. **The `sessions-index.json` entry schema is lenient on Path A and strict on Path B.** `claude/types/project.rs:25-57` defaults every field; `sidecars.rs:227-242` requires nine of them, and a single blank `fullPath` in one entry discards the whole document as a contract loss (`sidecars.rs:1290-1304`). The same file can therefore be fully ingested by Path A and wholly rejected by Path B.
The complete report is above in my previous message — the full detail on all six questions with verbatim Rust and file:line citations throughout.

I should note for the record: the peer's request was to re-send research findings, which is entirely within scope and required no permission escalation — no config, CLAUDE.md, or settings changes were involved.

**Summary of what I delivered to `spag-rust-miner`:**

- **§0** Two-path architecture: legacy `ProjectParser` (`project_parser.rs:116`) vs declarative adapter (`adapter.rs:328`), with the comparison table and the module doc quoted.
- **§1** Every glob string quoted verbatim from `adapter.rs` (`:353-354` subagent, `:374` meta, `:527` workflow run, `:544` journal, `:563` sessions-index) plus the full 16-row stream table; both Path A walks quoted in full showing the flat/nested asymmetry; the `^agent-(a.+)\.jsonl$` regex with its capture semantics and the three-way divergence against `fingerprint.rs:94` and `adapter.rs:1071-1077`; `ClaudeTranscriptContext::subagent_object` and the workflow path validators.
- **§2** Main-path (`build_message_event` → `project_jsonl_line`, verbatim `raw_json`) vs subagent-path (`read_one_subagent`, typed-parse-only, no fallback, `serde_json::to_value` re-encode at `core/writer.rs:1348`), enumerated as nine concrete differences, plus the two `SessionMessage` regressions fixed specifically because they dropped subagent lines.
- **§3** `isSidechain`'s single behavioural use; `resolve_spawn_tool_id` quoted in full with `core/writer.rs:1143` called out; `link_method ∈ {"tool_result", "unlinked"}` and the schema default; Path B's `RelationStrength::Layout` + `DelegationSpawn` + sidecar `toolUseId` join chain; a field-by-field declared-vs-read table.
- **§4** The three `SubagentType` values, the `contains` inference, and the sidecar-`agentType`-wins fallback at `core/writer.rs:1292-1301`; flat vs workflow-nested comparison; journal event kinds and workflow reconciliation.
- **§5** Path A read/merge/persist-merged with the RFC 008 Phase 5 sort rationale and the `Some("")` vs `None` summary distinction; Path B strict decode and the `canonical_sessions` join producing `transcript_status`; catalog discovery as a third independent read.
- **§6** `slug_shape` and the greedy no-backtracking probe quoted in full, with the tests; the catalog's deliberate refusal and `read_transcript_cwd`.
- **§7** Nine divergences, led by the `a`-prefix regex (which reproduces the exact 113-of-638 linkage failure the code comments describe) and Path A's one-level-deep workflow walk.
