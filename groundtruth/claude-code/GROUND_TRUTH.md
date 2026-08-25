# Claude Code — Ground Truth Reference

Curated catalog of everything rosetta knows about Claude Code's on-disk data.
Distilled 2026-08-24 from four evidence sources, strongest first:

1. **Fixtures** — hermetic corpus `fixtures/2.1.241/` (CI-enforced).
2. **Census** — `census/2026-08-24-local-structure.json`: structure-only census of
   a 3.9 GB organic tree (5,488 transcripts, 40 writer versions 2.1.37→2.1.241).
3. **spaghetti** (p008) — production parsers that ingested this format for months:
   `packages/sdk/src/types/claude/projects.ts` (the transcript schema, with
   real-data commentary), `docs/rfcs/011-claude-adapter-source-map.md` (formal
   stream catalog), `docs/PARSER-AUDIT-2026-07-02.md`, `docs/PARSER-UNPARSED-DATA.md`.
   Citations below are into that repo. Its docs' line numbers are stale — trust
   its TypeScript, not its docs' line references.
4. **Community/vendor docs** — code.claude.com/docs (30-day GC, headless flags),
   community schema write-ups (stale; superseded by the above).

Anthropic declares this format "internal … changes between versions". Nothing
here is a vendor contract; everything decays. The fixture farm + census refresh
are the maintenance mechanism.

---

## 1. Data root

`~/.claude`, relocatable via `CLAUDE_CONFIG_DIR`. Known top-level inventory
(spaghetti validate_config_and_settings.py + census):

- **Files**: settings.json, settings.local.json, stats-cache.json, history.jsonl,
  statusline-command.sh, mcp-needs-auth-cache.json, `.claude.json` (+ backups/),
  CLAUDE.md, .last-update-result.json, .last-cleanup (bare ISO timestamp),
  .credentials.json (**MUST-NOT-PARSE** — deliberately ignored, keep it that way).
- **Dirs**: projects, todos, tasks, plans, teams, sessions, file-history,
  shell-snapshots, backups, cache, debug, ide, paste-cache, plugins, session-env,
  statsig, telemetry — plus (census 2026-08, not in spaghetti's inventory:
  newer) uploads/, .idea/, vercel-plugin-* files.
- spaghetti hard-ignores for live ingestion: `debug/`, `telemetry/`,
  `paste-cache/`, `session-env/` (dominate event traffic, nothing live-relevant),
  suffixes `.tmp` (atomic-write staging), `.DS_Store`.

## 2. Transcript stream — `projects/<slug>/<sessionId>.jsonl`

Session files are UUID-named (`^[0-9a-f]{8}-…\.jsonl$`). `-c` continuation
appends to the same file (fixture-proven). 30-day GC via
`settings.cleanupPeriodDays`.

### 2.1 Envelope (BaseMessageFields)

Required: `type`, `uuid`, `parentUuid` (string|null), `timestamp`, `sessionId`,
`cwd`, `version` (writing CLI version — per-record version stamp), `gitBranch`,
`isSidechain`, `userType` ('external').
Optional: `slug` (doubles as plan-slug join key), `permissionMode`, `entrypoint`
('cli'|'web'|'mobile'|…), `sessionKind` ('bg' = background agent; can appear on
EVERY envelope type), `session_id` (snake_case duplicate of sessionId — Claude
Code emits both).

### 2.2 Record types (20 known; catch-all mandatory)

`user`, `assistant`, `system`, `summary`, `progress`, `attachment`,
`file-history-snapshot`, `file-history-delta`, `queue-operation`, `last-prompt`,
`mode`, `permission-mode`, `ai-title`, `custom-title`, `agent-name`,
`bridge-session`, `pr-link`, `frame-link`, `atis-latch`, `saved_hook_context`.
Census counts (30-day window): attachment 308k (most common!), assistant 155k,
user 88k, progress 46k, last-prompt 21k, permission-mode 21k, ai-title 17k,
mode 17k, system 14k, fh-snapshot 10k, bridge-session 7k, queue-operation 6k,
pr-link 4k, fh-delta 3k, **no-type-field lines 2k**, atis-latch 1k.
`ai-title` is ADDITIVE to `custom-title`, not a rename. `atis-latch` (opaque
anti-tamper token) first observed 2026-08-24 — the format moves week-to-week.

**user** — extends base: `message` {role:'user', content: string | blocks[]},
plus optional: `origin`{kind,…}, `thinkingMetadata`{level, disabled, triggers[],
maxThinkingTokens}, `todos`[], `toolUseResult` (see §3), `sourceToolAssistantUUID`,
`sourceToolUseID`, `agentId`, `isMeta`, `isCompactSummary`,
`isVisibleInTranscriptOnly`, `planContent`, `promptId`, `promptSource`
(typed|system|queued|suggestion_accepted), `imagePasteIds` (**string OR number**
array — see §8.1), `teamName`, `classifierMetaLines` (RAW STRING of
newline-delimited JSON), `interruptedMessageId`, `toolDenialKind`
(e.g. 'automode-blocked'), `queuePriority`, `userFeedback`.

**assistant** — extends base: `requestId` (absent in `--bare -p` fixtures →
treat optional), `message` payload {model, id, type:'message', role, content[],
stop_reason (end_turn|tool_use|stop_sequence|max_tokens|null), stop_sequence,
usage, context_management?{applied_edits[]}|null, container?}, plus optional:
`agentId`, `error`, `apiError`, `isApiErrorMessage`, `isAbortedMidStream`
(⇒ partial turn), `apiErrorStatus` (**number** — 429/400/401/404/529),
`errorDetails` (verbatim "<status> <json>" string), `teamName`,
`attributionMcpServer/Tool/Skill/Plugin`, `effort` ('low'…'xhigh').

**system** — 13 subtypes on `subtype` (base: level? info|error|suggestion,
isMeta?, hookAdditionalContext?[], pendingBackgroundAgentCount?,
pendingWorkflowCount?):
stop_hook_summary {hookCount, hookInfos[]{command}, hookErrors[],
preventedContinuation, stopReason, hasOutput, toolUseID} ·
turn_duration {durationMs, messageCount?} ·
api_error {cause, error{cause}, retryInMs, retryAttempt, maxRetries} ·
compact_boundary {content, logicalParentUuid, compactMetadata{trigger, preTokens}} ·
microcompact_boundary {content, microcompactMetadata{trigger, preTokens,
tokensSaved, compactedToolIds[]}} ·
local_command {content} · bridge_status {url?, content?} ·
away_summary {content} · informational {content?} · scheduled_task_fire {content} ·
model_consent_fallback {choice, fallbackModel, originalModel, persistedAsDefault} ·
model_refusal_fallback {content, trigger, direction, originalModel, fallbackModel,
requestId, apiRefusalCategory, apiRefusalExplanation, retractedMessageUuids[],
refusedUserMessageUuid|null} · agents_killed (marker).

**progress** — {data, toolUseID, parentToolUseID, agentId?, teamName?}; 7
`data.type` variants: hook_progress {hookEvent, hookName, command} ·
bash_progress {output, fullOutput, elapsedTimeSeconds, totalLines} ·
agent_progress {agentId, prompt, normalizedMessages[], message{…}} ·
mcp_progress {serverName, toolName, status started|completed, elapsedTimeMs?} ·
query_update {query} · search_results_received {query, resultCount} ·
waiting_for_task {taskDescription, taskType}.

**Markers/small**: agent-name {agentName}, custom-title {customTitle},
ai-title {aiTitle}, mode {mode}, atis-latch {atis},
permission-mode {permissionMode}, pr-link {prNumber:number, prUrl, prRepository,
timestamp}, frame-link {path, frameUrl, title, timestamp},
queue-operation {operation enqueue|dequeue|popAll|remove, timestamp, content?},
last-prompt (base + lastPrompt), saved_hook_context (base + content[] strings,
hookName, hookEvent, toolUseID) — all also carry sessionId.
**summary** {summary, leafUuid} — NO envelope fields.
**attachment** — base + attachment{type, hookName?, toolUseID?, hookEvent?,
content?, stdout?, stderr?, exitCode?, command?, durationMs?, …open}.
**file-history-snapshot** {messageId, isSnapshotUpdate, snapshot{messageId,
timestamp, trackedFileBackups: Record<path, {backupFileName|null, version,
backupTime, realParentDir?}>}} — backupFileName:null = EXPLICIT non-capture.
**file-history-delta** {messageId, snapshotMessageId, trackingPath, backup}.
**bridge-session** {sessionId, bridgeSessionId, lastSequenceNum,
ownerAccountUuid?, ownerOrganizationUuid?} — owner UUIDs are **SENSITIVE**: ok
in raw evidence, barred from FTS/logs/telemetry/runtime events (spaghetti drift
claim claude-drift-bridge-accounts).

### 2.3 Content blocks

Assistant (5): `thinking`{thinking, signature?} · `redacted_thinking`{data} ·
`text`{text} · `tool_use`{id, name, input} · `fallback`{from{model,…},
to{model,…}} — records an automatic model transition INSIDE one response.
User (4): `tool_result`{tool_use_id, content: string | [{type, text?, …}],
is_error?} · `text` · `document`/`image`{source{type:'base64', media_type,
data}} — reduce base64 to a hash in projections; raw stays in the ledger.
User content may also be a PLAIN STRING (census: 9.5k occurrences).

### 2.4 Tool names (43 + template)

Read Write Edit Glob Grep Bash Task TodoWrite TaskCreate TaskUpdate TaskList
TaskOutput TaskStop TaskGet WebSearch WebFetch NotebookEdit AskUserQuestion
EnterPlanMode ExitPlanMode Skill KillShell Agent Artifact ToolSearch
EnterWorktree ExitWorktree SendMessage CronCreate CronDelete CronList LSP
ListAgents TeamCreate TeamDelete Monitor PowerShell PushNotification
ReportFindings ScheduleWakeup SendUserFile Workflow StructuredOutput — plus
`mcp__<server>__<tool>` (split on `__`: server = part 1, tool = rest).
Treat as open vocabulary; never an exhaustive enum.

### 2.5 Usage (as recorded — superset of the API response)

input_tokens, output_tokens, cache_creation_input_tokens,
cache_read_input_tokens, cache_creation{ephemeral_5m_input_tokens,
ephemeral_1h_input_tokens}, service_tier|null, inference_geo?,
server_tool_use?{web_search_requests, web_fetch_requests} — plus
fixture-observed extras the CLI synthesizes: iterations[],
output_tokens_details{}, speed, stop_details. Accounting semantics: per-message
DELTA, native-exact (RFC 011).

## 3. toolUseResult — keep it opaque (production scar)

Per-tool result shapes are NOT modelled, deliberately. spaghetti once typed
`toolUseResult` strictly to the Read tool's shape
({type:'text', file:{filePath, content, numLines, startLine, totalLines}});
every other tool's result then failed typed deserialization and stored **empty
FTS text** — a silent production bug (PARSER-UNPARSED-DATA.md §"FTS recall
drift", fixed 2026-07-02). Rosetta rule: `toolUseResult: string | object`,
opaque `Value`; any future per-tool typing must be additive, evidence-backed,
and NEVER make narrowing failure fatal. (Tool *input* shapes are stable and
documented in spaghetti's viewer components if ever needed.)

## 4. Subagents & workflows — `projects/<slug>/<sid>/…`

- `subagents/agent-<agentId>.jsonl` (+ nested
  `subagents/workflows/<wf>/agent-*.jssonl` — one level). Agent ids start with
  'a' (regex `^agent-(a.+)\.jsonl$`).
- Own record vocabulary on top of the shared types (census): `started`,
  `result`, `fork-context-ref`.
- Sibling `agent-<id>.meta.json` (SubagentMeta): agentType (free-form — 22+
  real values; do NOT infer from filename), description?, name?, spawnDepth?,
  worktreePath?, parentAgentId?, model?, taskKind?, teamName?, color?,
  planModeRequired?, permissionMode?, stoppedByUser?, toolUseId? (join back to
  the spawning Agent tool_use). Nested workflow metas can be just
  {agentType[, spawnDepth]} — everything else optional (§8.5).
- `workflows/wf_*.json` (WorkflowRun): runId, workflowName, status, agentCount,
  totalTokens, totalToolCalls, durationMs + real-data extras (phases,
  result{confirmed, refuted_count, uncertain}, workflowProgress[]).
- `subagents/workflows/<wf>/journal.jsonl`: started/result lines that appear
  ONLY here (result embeds each subagent's verbatim StructuredOutput). A typed
  subagent-line reader would silently drop them — journals need their own
  reader.
- `tool-results/<toolUseId>.txt` — persisted tool results; stems are OPAQUE
  (toolu_*, short ids, hook ids). .json/PDF siblings exist and are knowingly
  excluded by spaghetti.

## 5. Other streams

- **sessions-index.json** (per project): {version, originalPath?, entries[]} —
  declared-required fields are ROUTINELY ABSENT on disk (firstPrompt);
  SUPPLEMENTAL authority only: never create sessions from it; index can outlive
  transcripts; created/modified ordering can surprise.
- **todos/`<sessionId>-agent-<agentId>`.json** — bare JSON ARRAY of
  {content, status pending|in_progress|completed, activeForm?}. Lazy-split the
  filename (ids may contain '-agent-'). Item identity EXCLUDES status.
- **tasks/`<sid>`/** — `.lock` (empty), `.highwatermark` (integer), `{N}.json`
  items {id, subject, description, activeForm?, owner?, status, blocks?[],
  blockedBy?[]}. Directory name is NOT always a session/team id.
- **plans/`<slug>`.md** — title = first H1 else slug. Envelope `slug` field +
  user `planContent` join here.
- **file-history/`<sid>`/`<hash>@v<N>[.ext]`** — extension tail optional
  (live parser observed it; strict regex misses files).
- **projects/`<slug>`/memory/*.md** — every immediate .md is an independent
  object; MEMORY.md is convention, not a complete snapshot.
- **teams/`<team>`/config.json** — {name, description?, createdAt, leadAgentId,
  leadSessionId, members[]}; member {agentId "{name}@{team}", name, agentType?
  (lead only), model?, prompt?, color?, planModeRequired?, joinedAt, tmuxPaneId
  (''|'leader'|'in-process'|pane id), cwd, subscriptions[], backendType?}.
  A directory IS a team even with missing/corrupt config (observed) →
  config:null. **inboxes/`<agent>`.json** — JSON ARRAY of {from, text (may embed
  JSON — leave raw), summary?, timestamp, color?, read, msg_id?, msgV?, type?}.
  Payload variants seen: idle_notification, task_assignment, shutdown_request,
  shutdown_approved. Identity: msg_id, else fingerprint(sender+timestamp+text
  +ordinal); `read` excluded from identity.
- **sessions/`<pid>`.json** — active-session presence: pid, sessionId, cwd,
  startedAt (+ kind, entrypoint, name, nameSince, status busy|idle|shell,
  updatedAt, statusUpdatedAt, procStart, version, peerProtocol, peerFeatures[],
  nameSource, bridgeSessionId, messagingSocketPath). Identity = pid + sessionId
  + proc-start marker (PID reuse). Registry removal is NOT terminal run state.
  `nameSince` semantics unproven — never use as ordering/lifecycle timestamp.
  `sessions/<pid>.<hash64>.key` sidecars: inventory size only — **NEVER expose
  key material**.
- **settings.json / settings.local.json** — same schema; LOCAL WINS on merge and
  consumers must apply it. Keys incl. permissions{allow[]}, model (ALIAS like
  `opus[1m]`, not a bare id), effortLevel, hooks{}, cleanupPeriodDays,
  autoCompactEnabled, statusLine, env, tui, … `autoMode` {environment[],
  allow[], soft_deny[]} is **SENSITIVE native-only** (drift claim
  claude-drift-auto-mode). spaghetti policy: settings bytes are
  "measure_only" — never copy raw into evidence stores.
- **stats-cache.json** — derived analytics the CLI maintains: dailyActivity[],
  dailyModelTokens[] (own sub-version field), modelUsage per model
  {inputTokens, outputTokens, cacheRead/Creation, webSearchRequests, costUSD,
  contextWindow, maxOutputTokens}, totals, hourCounts. Derived — corroboration,
  not canonical.
- **history.jsonl** — {display, pastedContents: Record<id, {id, type, content?,
  contentHash?}>, timestamp, project, sessionId}. MIGRATION IN FLIGHT: old
  entries carry content, new carry contentHash (→ paste-cache/<hash>.txt),
  never both.
- **Analytics tier** (hard-ignored live; still ground truth): telemetry/
  1p_failed_events.*.json = JSONL-despite-.json, 99 known tengu_* event names
  (open vocabulary), event_data.experiment_metadata/user_attributes are
  JSON-encoded STRINGS; statsig/ cached evaluations (data = JSON string,
  double-parse); debug/<sid>.txt + `latest` symlink (grammar: ISO8601 [LEVEL]
  msg, "at " → stack traces); shell-snapshots/snapshot-<shell>-<ts>-<hash>.sh;
  ide/<pid>.lock {workspaceFolders[], pid, ideName, transport,
  runningInWindows, authToken}; backups/*.json.backup (open-keyed global
  state); plugins/ (installed_plugins.json, marketplaces, cache tree);
  cache/ (changelog.md, my-closed-issues.json — unstable shape).

## 6. Project slug encoding

POSIX `/Users/me/app` → `-Users-me-app`; Windows `D:\Projects\app` →
`D--Projects-app` (older builds `D:-Projects-app`). Detection is
platform-independent (a synced tree decodes anywhere): leading '-' = POSIX;
`<letter>--`/`<letter>:-` = Windows drive. Decoding is ambiguous (dashes in
real names) — resolve by greedy longest-match filesystem probing. >200-char
paths: truncated + hashed (vendor-documented).

## 7. Sensitivity register

Never let these past the raw ledger into FTS/logs/telemetry/derived facts:
`bridge-session.ownerAccountUuid/ownerOrganizationUuid` · `sessions/*.key`
sidecars (never read content at all) · `.credentials.json` (never parse) ·
`settings.autoMode` (measure shape only) · `ActiveSessionFile.nameSince`
(opaque; no semantics) · base64 image/document bodies (hash in projections).

## 8. Port pitfalls (spaghetti's differential-oracle scars — do not repeat)

1. **TS types are documentation, not validation.** `imagePasteIds` was declared
   string[] but real data has numbers; a strict Vec<String> port dropped whole
   records. Rosetta: permissive types + flatten-unknown everywhere.
2. **toolUseResult over-narrowing** gutted FTS silently (§3).
3. **Directory listings must be sorted** before merging — NTFS returns sorted,
   APFS/ext4 don't; an unsorted merge diverged per developer machine.
4. **Missing catch-all on type/subtype enums nulled 4,900 real lines' FTS** —
   new discriminants failed the typed parse. Rosetta's parse_line routes
   unknown types to UnknownType (soft) by design; keep it that way for subtypes
   too.
5. **Required-vs-optional asymmetry**: SubagentMeta.description required in
   Rust would have errored on all 322 nested workflow metas. Default to
   Option + serde defaults on anything not fixture-proven required.
6. Streaming line-index rebasing: an incremental reader restarting msg_index at
   0 OVERWROTE the head of active sessions (upsert on (session, index)). Cursor
   design must rebase (MAX+1) and snapshot stat before parse (TOCTOU).
7. Stray files under projects/ are not slugs (.DS_Store) — directories only.
8. Don't collapse small closed vocabularies (stop_reason, todo status,
   queue operation, mcp status) to bare String in the canonical model — keep
   enums WITH catch-all variants; spaghetti lost exhaustive matching.

## 9. Known unknowns (observed, not yet handled anywhere)

hooks/ executables (referenced by settings.hooks[].command) · root CLAUDE.md ·
tasks item bodies unread by spaghetti (rosetta: read them) · backups/ unparsed ·
plugins/data/<plugin>-<mkt>/ (0-byte dirs) · projects/<slug>/vercel-plugin/
skill-injections.jsonl {matchedSkills, injectedSkills, droppedByCap, toolName,
toolTarget, contextChunks} · session-env scripts (268 real; filenames only) ·
uploads/ (census 2026-08; post-spaghetti) · `summary` records absent from the
census's 30-day window (older sessions only?) — confirm before assuming dead.
