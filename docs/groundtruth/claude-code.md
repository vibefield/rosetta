# Claude Code — Ground Truth Reference

Curated catalog of everything rosetta knows about Claude Code's on-disk data.
Distilled 2026-08-24 and refreshed through 2026-09-03 from four evidence
channels, strongest first:

1. **Fixtures** — hermetic corpus `fixtures/2.1.241/` (CI-enforced).
2. **Census** — `groundtruth/claude-code/sources/observed-2026-08-25.json` is
   privacy-bounded organic structure scoped to writer version 2.1.241;
   `sources/observed-2026-09-03.json` is an explicitly historical, all-retained-version
   corroboration census covering delegated-work relations and workflow streams. The older
   `groundtruth/claude-code/census/2026-08-24-local-structure.json` records only
   aggregate version distribution across the full historical tree and is not merged.
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

### 2.4.1 `task` is an overloaded product word

Native spelling does not establish canonical identity. Claude Code has at least
two unrelated namespaces that use the word `task`, plus separate workflow and
team rails:

| Native evidence | Meaning | Canonical treatment |
|---|---|---|
| `TaskCreate`, `TaskUpdate`, `TaskList`, `TaskGet` | Shared work-item/task-board state: subject, status, dependencies, and ownership | Eligible for canonical task projection and `timeline_meta.tasks` when the payload proves a work item |
| `TodoWrite` | Legacy/session TODO state | Eligible only through its evidence-backed TODO/task projection |
| `TaskOutput`, `TaskStop` | Read or terminate a **background execution handle** | Process/delegate/team lifecycle; never a task-board row merely because the tool name begins with `Task` |
| `Agent`, `ListAgents`, `SendMessage`, `TeamCreate`, `TeamDelete` | Worker or team lifecycle and communication | Delegation/team rails |
| `Workflow` | Workflow orchestration | Workflow rail |

`TaskStop` does not identify one kind of entity. Its result carries the native
`task_type`; observed values include `local_agent` (ordinary background
subagent), `in_process_teammate` (an Agent Team member), and `local_bash`
(background shell execution). It is therefore an execution-control primitive,
not a synonym for workflows, subagents, teams, or shared tasks. The discriminator
and relationship joins decide which lifecycle rail owns the event. A stop also
does not prove that a shared work item became completed, cancelled, or otherwise
changed state. For teams, the shared task list and teammate lifecycle remain
separate; a teammate shutdown request is distinct from updating its work item.

**Evidence scar (2026-09-04).** Native session
`becd74f6-09e8-414f-8862-36094340c0be` contained eight `TaskStop` calls and no
`TaskCreate`, `TaskUpdate`, `TaskList`, `TaskGet`, `TodoWrite`, or canonical task
rows. The stop results classified two handles as `local_agent` and six as
`in_process_teammate`. Some request handles were readable aliases while the
result supplied a different runtime id (for example `spag-rust-miner` →
`tz6wjatgk`). Treating every adapter `ToolAction::Task` as a work item produced
exactly the bad UI signature: eight id-as-title rows with no state, start time,
or duration.

**Projection law.** `timeline_meta.tasks` admits only source-proven work items or
rows already present in the canonical task table. `TaskOutput` and `TaskStop`
must normalize as execution/delegation control, or remain bounded unknowns when
their kind cannot be proven. They must not enter the task-board candidate set.
This rule is semantic and payload-driven; a tool-name prefix is insufficient.

Official semantic cross-checks: [tools reference](https://code.claude.com/docs/en/tools-reference),
[subagents and background tasks](https://code.claude.com/docs/en/sub-agents), and
[agent teams](https://code.claude.com/docs/en/agent-teams).

### 2.5 Usage (as recorded — superset of the API response)

input_tokens, output_tokens, cache_creation_input_tokens,
cache_read_input_tokens, cache_creation{ephemeral_5m_input_tokens,
ephemeral_1h_input_tokens}, service_tier|null, inference_geo?,
server_tool_use?{web_search_requests, web_fetch_requests} — plus
fixture-observed extras the CLI synthesizes: iterations[],
output_tokens_details{}, speed, stop_details. Accounting semantics: per-message
DELTA, native-exact (RFC 011).

## 3. toolUseResult — raw stays permissive; projection may narrow (production scar)

The **raw Claude type** is not narrowed to one per-tool result enum. spaghetti once typed
`toolUseResult` strictly to the Read tool's shape
({type:'text', file:{filePath, content, numLines, startLine, totalLines}});
every other tool's result then failed typed deserialization and stored **empty
FTS text** — a silent production bug (PARSER-UNPARSED-DATA.md §"FTS recall
drift", fixed 2026-07-02). Rosetta raw rule remains
`toolUseResult: string | object`, represented permissively and drift-reported.

The canonical projection has a different job: an adapter-owned, evidence-backed semantic
projector may extract a bounded common action (for example Read → path/range, Bash →
command/status, Edit → path/one diff) while dropping native result noise. That projector
must be downstream of successful raw admission. Narrowing failure NEVER fails the record or
stores an empty substitute. A known typed request survives an unfamiliar completion with its
outcome omitted plus ledger provenance/drift; only an unregistered tool or a request missing
its minimum identity becomes a bounded `unknown` action. The raw ledger retains the complete
value for replay. This separation keeps the production scar intact without making opaque
native JSON a public presentation contract.

## 4. Delegated work — three persisted relationship geometries

The 2026-09-03 structure-only audit covered 562 main transcripts, 4,268 direct
worker transcripts, 760 workflow-managed worker transcripts, 48 workflow journals,
2,765 worker sidecars, 44 team configs, 30 inbox files, and 47 retained task records.
Those rails overlap, but they are not synonyms. In particular, `subagent` is a broad
product word, while ordinary Agent workers, Agent Team members, and Workflow workers
have different native ownership and messaging evidence.

### 4.1 Ordinary Agent workers

- Direct transcripts are `subagents/agent-<agentId>.jsonl`. Sibling
  `agent-<id>.meta.json` files carry optional `agentType`, description, name,
  spawnDepth, worktreePath, parentAgentId, model, taskKind, teamName, color,
  planModeRequired, permissionMode, stoppedByUser, isFork, and toolUseId. The
  enclosing main-session directory proves the owning session; `toolUseId`, when
  present, is the exact invocation join. Of 4,268 direct worker files, 2,263 have
  no sidecar, so historical linkage cannot depend on the sidecar alone.
- An ordinary worker is not a write-only one-call/one-result channel. User records
  contain 258 structured `origin.kind=peer` deliveries: 178 worker-to-main and 73
  worker-to-worker records use an `agent-message` wrapper, and seven additional
  cross-session/process peer deliveries carry the structured origin without that
  wrapper. Another 25 `origin.kind=coordinator` records deliver follow-up prompts to
  24 already-existing workers.
- Four stored `Agent` requests use `resume`; their results have
  `status=teammate_spawned`, scoped to the existing named worker. Separately, 88
  SendMessage results carry `resumedAgentId` (51 from team workers, 24 from main
  transcripts, 13 from non-team workers). `teammate_spawned` is therefore a transport
  result, not sufficient proof of a first join; the initiating request distinguishes
  launch from resume.
- Fifty direct workers have `spawnDepth=2`. Five sidecars are forks, 13 record a
  worktree path, and three record `stoppedByUser`. A child file may contain the same
  user/assistant/tool grammar as its parent plus `fork-context-ref`; relation and
  lifecycle must not be inferred from transcript prose.
- Runtime identity is response-scoped evidence. Seventy-five worker files contain more
  than one observed model/effort pair, while 82 direct workers contain no assistant
  response at all. Sidecar/configured model aliases are useful configuration facts but
  cannot fill a missing response model or effort.

### 4.2 Agent Teams

- `teams/<team>/config.json` stores `{name, createdAt, leadAgentId, leadSessionId,
  members[]}`. A member has an address (`name` and composite `agentId`), optional
  configured model, prompt, native color, `planModeRequired`, join time, cwd,
  subscriptions, and backend/pane fields. The retained configs contain all eight native
  colors: blue, green, yellow, purple, orange, pink, cyan, and red. Color is optional;
  missing is real absence, not permission to synthesize a native color. The local corpus
  proves only `in-process` members; tmux/split-pane presentation remains unvalidated.
- Team communication is stored as one or more top-level `teammate-message` envelopes in
  a user record. The direct envelope census is 1,904: 1,579 plain messages, 120
  `idle_notification`, 190 `task_assignment`, three `shutdown_request`, six
  `shutdown_approved`, and six `teammate_terminated`. `idleReason` includes `available`
  and `failed`; availability is a state signal, while a failed idle carries failure
  evidence. There is no native `report` message kind: substantive reports remain ordinary
  `message` payloads, and report-like prose alone is not a structural classifier.
- The raw corpus contains 3,220 literal `teammate-message` tags. The extra 1,316 are
  nested inside another top-level task/result envelope rather than independent messages.
  A parser must recognize only top-level envelopes for chronology and retain nested copies
  as provenance, not replay them as additional speech.
- Additional local evidence (2026-09-09, Claude 2.1.266): a cross-session delivery
  stores `Another Claude session sent a message:` before a batch of four top-level
  envelopes, followed by Claude's fixed `This came from another Claude session …`
  permission notice. This record has no `origin` or `teamName`. The exact preface
  and notice are transport, not human speech; the enclosed messages and idle
  notifications retain their own authorship and kinds. Normalizer 23 recognizes
  this framing without treating arbitrary surrounding prose as delivery evidence.
- `teams/<team>/inboxes/<agent>.json` is a retained mailbox array. Its 190 entries split
  into 165 read and 25 unread; the unread set contains ten plain messages and 15 task
  assignments. Unread inbox-only items are pending delivery state and have no justified
  position in delivered transcript chronology.
- Ten `taskKind=in_process_teammate` sidecars at `spawnDepth=1` sit under one ordinary
  worker, proving that an Agent worker can own a team. Runtime error text also records 65
  rejected attempts by teammates to spawn teammates. Both are historical facts: readers
  must honor stored ownership evidence rather than hard-code either a permanently flat or
  permanently recursive team topology.

### 4.3 Workflow-managed workers

- Worker transcripts and minimal sidecars live under
  `subagents/workflows/<workflow>/agent-*.jsonl`. All 760 child files are non-empty and
  together contain 35,545 records: 21,585 assistant, 11,378 user, and 2,582 attachment.
  Their 760 sidecars comprise 655 `{agentType, spawnDepth}` shapes and 105
  `{agentType}` shapes; the observed agent types are 672 `workflow-subagent`, 87
  `general-purpose`, and one `Explore`. Every journaled start has a corresponding child
  transcript. The `<workflow>` directory segment is the exact owning run id and survives
  bounded metadata normalization on the child relation. It is stronger than the generic
  parent-session fallback: even while pagination leaves the run message off-page, a workflow
  child must wait for and render inside that run's phase/worker row, never as a standalone
  subthread in the parent chronology. Current roster references and journal agent ids jointly
  retain ownership across resumed runs, including workers from superseded attempts.
- The child transcript is the authoritative worker chronology, not the preview fields in
  the run snapshot. Seven hundred forty-one children contain a real assistant model; 19
  contain only synthetic API-error assistant records, all stale workers from an earlier
  attempt of one resumed run. Seventeen child files record `effort=max`; the other 743
  record no effort. Excluding synthetic error records, each child has exactly one observed
  model and every current worker that can be correlated agrees with its configured progress
  model after removing the `[1m]` context alias.
- `workflows/wf_*.json` is a mutable WorkflowRun snapshot. All 48 observed documents carry
  run/task identity, `workflowName`, summary, full inline script and `scriptPath`, declared
  phases, `defaultModel`, current `workflowProgress`, timestamps/duration, logs, aggregate
  token/tool counts, status, and result; five also carry `args` and three terminal errors.
  The 48 retained runs are 45 completed, one failed, and two killed. `timestamp` agrees with
  `startTime + durationMs` to within five milliseconds, and `agentCount` equals the number
  of current `workflow_agent` progress records in every snapshot.
- `workflowProgress` is a replaceable current roster, not an event stream. It contains 119
  `workflow_phase` and 761 `workflow_agent` records. Worker states are 714 `done`, six
  `error`, 13 `progress`, and 28 `start`. Every worker has a configured model and none has
  effort. All 28 `start` rows belong to one killed run and lack an agent id/start time,
  attempt, metrics, duration, and result: they mean queued while live and never started once
  the run is terminal. The 13 `progress` rows belong to killed runs and become interrupted,
  not perpetually running. A completed run can still contain worker errors: one observed
  completed run has 19 done and five errored workers plus a final result.
- Fourteen current workers in the two resumed runs carry `cached=true`. Cached rows represent
  reused prior work, omit fresh attempt/timing/usage fields, and can have an empty result even
  when a durable child transcript exists. They must not be presented as newly executed work.
  Worker labels are display text, not identity: 11 runs reuse labels, with 71 occurrences
  beyond the first. Join current progress, journal, and children by run/key/index/agent id,
  never by label.
- A run's terminal `result` is workflow-defined JSON, not one fixed result schema: the 45
  completed snapshots contain 43 objects and two arrays; failed/killed snapshots contain
  null plus an error string. `promptPreview`, `resultPreview`, `lastToolName`, and
  `lastToolSummary` are explicitly bounded/current hints. Logs are arrays of untimestamped
  strings. Neither can replace a child transcript or establish event chronology.
- `subagents/workflows/<workflow>/journal.jsonl` is a distinct append-only stream. It has
  760 `{type:"started", agentId, key}` records and 714
  `{type:"result", agentId, key, result}` records: 659 result objects and 55 strings. Journal
  rows have no timestamp, status, error, model, or usage fields. There are 46 unmatched
  starts and no unmatched results. Two resumed journals retain 27 starts from superseded
  attempt rosters, while one killed run's current roster contains 28 queued rows that never
  received an agent id or journal start. The journal is therefore cumulative lifecycle
  evidence, not a balanced list, clock, denominator, or current roster.
- The Workflow tool has 53 stored calls: 51 launches and two resumes. Fifty accepted attempts
  returned a plain-text launch receipt in the visible `tool_result` block and a parallel
  structured `toolUseResult` with `status=async_launched`, task id, run id, transcript/script
  paths, and summary; three validation errors remained plain text and created no task or run.
  The two resumes produced new task ids while retaining their original run ids. Inputs cover
  inline script, description plus script, `resumeFromRunId` plus `scriptPath`, and
  argument/name/path variants. Workflow is thus a third ownership rail, not an Agent Team
  alias or a single opaque ledger row.

### 4.4 Arrival, correlation, and de-duplication constraints

- Top-level `task-notification` envelopes record asynchronous arrival independently of
  the original tool line. The retained direct-envelope census is 1,591: 1,291 completed,
  68 failed, 44 killed, three stopped, and 185 without a status. Optional fields include
  result, usage, output file, diagnostics, worktree, workflow-agent counts, and duration.
- Forty-six of the 50 accepted Workflow attempts have an exactly correlated top-level
  notification: 44 completed and two failed. Thirty-seven completed result bodies are
  explicitly truncated with a pointer to a temporary full-result file, seven are complete,
  and the two failed callbacks omit result while retaining usage. All 46 referenced temp
  output files had expired at audit time. The four missing callbacks comprise both killed
  runs, one completed standalone run, and the first attempt of a later-resumed run; a
  callback is therefore optional arrival evidence, never the durable result store.
- Workflow callbacks and snapshots have different scope. In one real run the first attempt's
  callback failed, a resume later completed the same run, and the current snapshot's `taskId`,
  status, and aggregate metrics describe the resumed attempt. The other correlated callbacks
  agree with their current snapshots. Preserve callback status/usage on the exact attempt;
  never let an earlier failed callback terminalize the resumed run or overwrite its final
  metrics. The snapshot retains only the latest task id, so attempt ids must be accumulated
  from the chronological tool calls/results.
- The same team communication can be present in four places: sender SendMessage input,
  structured routing result, retained inbox entry, and recipient envelope. Use native
  message/routing/tool/request/task identifiers and timestamps to correlate these
  observations; text equality is not identity. Of 2,008 SendMessage requests, 1,441 carry
  both `content` and `message`; at least 1,000 pair a 50-character preview in `content`
  with a longer full `message`, so the latter is the canonical body when both exist.
- Historical classification must fail closed but use all exact evidence available:
  Agent input/result, sidecar fields, team config/inbox membership, storage path,
  structured origin, and workflow journal. Neither a filename nor
  `status=teammate_spawned` alone establishes the relationship kind.
- `tool-results/<toolUseId>.txt` stores persisted tool output under opaque stems.
  `.json` and PDF siblings exist and remain outside the old spaghetti reader; their
  presence is not a relation signal.

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
  Direct envelope/control variants are plain message, idle_notification,
  task_assignment, shutdown_request, shutdown_approved, and teammate_terminated.
  For stable identity inside one inbox use msg_id, else a full-record fingerprint
  including sender, timestamp, text, and ordinal; `read` is mutable and excluded.
  That fallback must never correlate a separate sender/result/recipient observation
  by text equality (§4.4).
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
