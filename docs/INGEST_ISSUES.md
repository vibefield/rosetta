# Ingest issues

This document records confirmed ingestion correctness defects and their
resolution. Local organic data may corroborate an issue, but regression coverage
must use a minimized, sanitized fixture rather than depending on a developer's
live agent data.

## INGEST-001 — Resolved: Claude Code titles fell between bounded skim windows

| Field | Value |
|---|---|
| Status | Resolved |
| Recorded | 2026-08-28 |
| Agent | `claude-code` |
| Affected paths | Bounded meta ingest and later title reconciliation |
| Previous impact | A real session title was stored as `NULL` and rendered as “No title captured” |

### Summary

Claude Code stores generated and explicit session titles as `ai-title` and
`custom-title` records inside the session transcript. Rosetta's meta phase reads
only a fixed head and tail window from append-only JSONL files. A valid title can
sit outside both windows even when it appears hundreds of times in the file.

The failure was persistent rather than merely a delayed meta-view result. The
resolution now covers both bounded metadata extraction and full-stream
convergence.

### Confirmed case

Native session:

```text
becd74f6-09e8-414f-8862-36094340c0be
```

Observed transcript facts:

| Fact | Observed value |
|---|---:|
| Transcript size | 33,220,230 bytes |
| JSONL records | 14,292 |
| `ai-title` records | 503 |
| Native role | Interactive main session |
| Nested subagent sessions | 41 |
| Expected title | `Project rosetta multi-agent data ingestion engine` |
| Projected title after meta ingest | `NULL` |

This is not an empty or bridge-only transcript. The active-session metadata calls
it `kind: "interactive"`; a remote-control `bridgeSessionId` associated with that
interactive session does not change the native transcript's identity or contents.

### Exact boundary failure

Default bounds in `crates/pipeline/src/lib.rs` are:

```text
head: 64 KiB = 65,536 bytes
tail: 16 KiB = 16,384 bytes
```

The relevant zero-based byte offsets in the confirmed transcript were:

| Record/window | Offset or interval | Result |
|---|---:|---|
| Head window | `[0, 65,536)` | No title record |
| First `ai-title` | `67,040` | 1,504 bytes after the head window |
| Last `ai-title` | `33,198,507` | 21,723 bytes before EOF |
| Tail window | `[33,203,846, 33,220,230)` | Starts 5,339 bytes after the last title |

The first twenty JSONL records occupy more than 64 KiB because several opening
records are large. Near EOF, a small number of post-title records occupy more than
16 KiB. The title therefore lands in the uninspected middle even though it is
repeated throughout the transcript.

Equivalent local corroboration commands are:

```sh
wc -l "$TRANSCRIPT"
rg -c '"type":"ai-title"' "$TRANSCRIPT"
rg --byte-offset --line-number '"type":"ai-title"' "$TRANSCRIPT" | head
rg --byte-offset --line-number '"type":"ai-title"' "$TRANSCRIPT" | tail
```

### Previous code path

1. `IngestOptions::default` requests a 64 KiB head and 16 KiB tail.
2. `meta_ingest_into` passes only those byte slices to `AgentAdapter::skim`.
3. `ClaudeCodeAdapter::skim` searches the parseable lines in those slices for
   `aiTitle` and `customTitle`.
4. Neither window contains a title in this case, so `SkimResult.title` is `None`.
5. `admit_identified` writes that value into `SessionRow.title`, producing SQL
   `NULL` in the meta projection.
6. During full ingest, `ai_title_line` and `custom_title_line` emit canonical
   `MessageKind::Summary` rows only. No later operation updates
   `SessionRow.title`, so streaming does not repair the missing value.

Relevant implementation points:

- `crates/pipeline/src/lib.rs` — default skim bounds.
- `crates/pipeline/src/meta.rs` — bounded capture and session admission.
- `crates/adapters/claude-code/src/adapter.rs` — head/tail title extraction.
- `crates/adapters/claude-code/src/normalize.rs` — streamed title records become
  summary messages.
- `crates/store/src/apply/rows.rs` — session metadata upsert behavior.

### Implemented resolution

- `AgentAdapter::wants_skim_fallback` opts only main Claude transcripts with a
  missing title into extra metadata IO.
- Capture scans complete delimiter-framed records from the head boundary under
  explicit 128 KiB / 256-record caps. `skim fallback bytes` and `skim fallback
  time` are separately profiled.
- The fallback keeps searching after an `ai-title` for a higher-priority
  `custom-title`; malformed or budget-clipped records are never guessed at.
- Streamed `ai_title` and `custom_title` summary singletons now reconcile the
  public `session.title`. The store derives custom-over-generated precedence
  in RAM, persists only the selected title plus its bounded winning provenance,
  and uses same-session raw-ledger replay after a source replacement. Losing
  title candidates never become a second SQLite fact set.

The minimized boundary regression places the first title beyond 64 KiB and the
last beyond the 16 KiB tail, verifies the byte cap, then disables the fallback
and proves full streaming reaches the same explicit title. The 2026-08-28
real-source smoke projected the confirmed session as
`Project rosetta multi-agent data ingestion engine` while reading only 20.1 MiB
of fallback data across the entire Claude root, not its 33.2 MB transcript body
for metadata.

### Related ingest-bench display issue

The meta viewer's “Source files” value currently counts `ingest_cursor` rows.
Meta-only ingestion has not created stream cursors, so this large transcript is
shown as having zero source files. Workspace observations are likewise emitted
during line normalization, and skimmed agent versions are retained on transient
`SessionFile` values rather than persisted into the meta projection. These empty
fields do not mean that the native transcript is missing, but the labels make the
title failure look like a missing-source failure. That display/plumbing problem
should be tracked separately from title recovery.

## INGEST-002 — Resolved: Codex native title stores were not ingested

| Field | Value |
|---|---|
| Status | Resolved |
| Recorded | 2026-08-28 |
| Agent | `codex` 0.150.1 |
| Affected paths | Meta ingest, full streaming, and replay |
| Previous impact | Every Codex session was projected with `session.title = NULL` |

### Summary

Codex rollout headers correctly report `title: None`; they do not carry a
display title. The previous adapter incorrectly treated that one file family as
the complete data root and ignored title-bearing session metadata in
`state_5.sqlite`.

Unlike INGEST-001, this is not a boundary-window miss. Titles are suppressed by
construction, so increasing the rollout skim windows cannot help.

### Confirmed result

The Codex-only meta run on 2026-08-28 produced:

| Projection fact | Value |
|---|---:|
| Rollout sessions discovered | 381 |
| Main sessions | 221 |
| Subagent sessions | 160 |
| Sessions with a projected title | 0 |

The same live Codex data root contained:

| Native fact | Value |
|---|---:|
| `state_5.threads` rows | 381 |
| Rows with non-empty `threads.title` | 347 |
| Rows with non-empty `threads.name` | 7 |
| `session_index.jsonl` records | 13 |
| Unique sessions in `session_index.jsonl` | 8 |

Therefore at least 347 of the 381 projected sessions have a native fallback
title, and seven have a concise generated name in the current state database.
The remaining 34 rows have no native title and may honestly remain untitled.

Examples from the confirmed corpus:

| Native session | Generated name |
|---|---|
| `01a04979-fc5c-7310-a934-b66e339c063c` | `Review Rosetta codebase` |
| `01a0497a-eacd-7ef0-9e72-e8909af353a3` | `Onboard to Rosetta codebase` |
| `01a046b1-f069-7cc2-9303-e813236f27ae` | `Clarify agent file edit tracking` |

### Native title semantics

The two native surfaces serve different purposes:

- `state_5.sqlite:threads` is the complete current metadata projection. Its
  `name` column contains the concise generated name when one exists. Its `title`
  column is normally the first user message and is present for older sessions
  that predate the current session index.
- `session_index.jsonl` is a sparse append-only name update log. In the observed
  0.150.1 data it first records an immediate, at-most-36-character display name,
  then appends a concise generated name a few seconds later. The newest entry for
  a session wins.

`session_index.jsonl` cannot be the only bootstrap source: it currently covers
only eight unique sessions while `state_5.sqlite` covers all 381. Conversely,
copying `threads.title` verbatim is unsafe for display. In the confirmed database:

| `threads.title` statistic | Value |
|---|---:|
| Average length | 17,819 characters |
| Maximum length | 76,537 characters |
| Longer than 1,000 characters | 168 rows |
| Equal to `first_user_message` | 346 of 347 titled rows |
| Equal to `preview` | 346 of 347 titled rows |

Large values include injected context and other material that is valid prompt
history but not a usable session-list title. Generated `threads.name` values in
the confirmed database are 23–35 characters long.

The canonical display-title precedence should therefore be:

1. non-empty `threads.name` (or the latest equivalent
   `session_index.thread_name` update);
2. otherwise, a whitespace-normalized native display fallback derived from
   `threads.title`, capped to the observed Codex 36-character behavior;
3. otherwise, `None`.

The exact truncation rule must be defined over Unicode characters rather than
raw UTF-8 bytes and covered by fixtures. Rosetta must not persist an entire first
prompt merely because Codex calls that database column `title`.

### Previous code path

1. `groundtruth/codex/layout.toml` declares rollout files and a broad
   `state-dbs` family. It does not declare `session_index.jsonl`.
2. `CodexAdapter::stream_driver` enables only the `rollout` stream. Every state
   database is therefore inventory-only and is never queried.
3. `CodexAdapter::skim` reads the rollout's opening `session_meta`, then
   explicitly returns `title: None`.
4. `normalize::session_meta` also emits `SessionRow { title: None, ... }` during
   full streaming.
5. No later Codex operation patches the session title, so full ingest and replay
   preserve the null value.

The Codex-only benchmark corroborates this routing: 381 rollout files were
skimmed while 62 matched files were classified as inventory-only. The resulting
381 sessions all had null titles.

The implementation comment that “nothing codex writes names a session” is true
only of the rollout vocabulary. It is contradicted by the adapter's own Layer-1
schema evidence: `groundtruth/codex/sources/0.149.1.sqlite-schema.sql` declares
`state_5.threads.title` and `state_5.threads.name`.

Relevant implementation points:

- `groundtruth/codex/layout.toml` — rollout and state-database discovery.
- `groundtruth/codex/sources/0.149.1.sqlite-schema.sql` — native `threads`
  metadata schema.
- `crates/adapters/codex/src/adapter.rs` — rollout-only driver and null skim
  title.
- `crates/adapters/codex/src/normalize.rs` — streamed session row also has a
  null title.
- `crates/pipeline/src/meta.rs` — existing `SqliteSnapshot` admission path that
  can enumerate sessions from an agent-owned database.

### Implemented resolution

- Ground truth now routes `state_*.sqlite*` ahead of the broad SQLite inventory
  and drives it through the existing stable online-backup snapshot path.
- `SELECT * FROM threads` is deliberate compatibility SQL: the adapter inspects
  optional `name` and millisecond timestamp columns when present and falls back
  to the stable legacy columns without ever referring to a missing column in
  SQL. A missing `threads` table remains an explicit compatibility error.
- State rows and rollouts derive the same canonical id. Native `name` wins;
  otherwise Unicode whitespace is normalized and `title` is truncated to 36
  Unicode scalar values including an ellipsis. Blank native data stays `None`.
- State snapshot changes re-run metadata admission, so a later generated name
  replaces the fallback. A title-less rollout merge cannot erase it, and the
  metadata op is journaled for rebuild.

Hermetic tests cover current and legacy row shapes, Unicode truncation, parent
recovery, one-id merging, title-less rollout streaming and incremental name
updates. The 2026-08-28 real-source schema-6 smoke projected 348 titled Codex
sessions out of 382, with a maximum title length of 36 characters. The three
confirmed generated names above appeared exactly; the other 34 sessions remain
honestly untitled.

## INGEST-003 — Resolved: Copilot workspace titles were inventory-only

| Field | Value |
|---|---|
| Status | Resolved |
| Recorded | 2026-08-28 |
| Agent | `copilot` 1.0.80 |
| Affected paths | Meta ingest, full streaming, and replay |
| Previous impact | Copilot sessions were projected with `session.title = NULL` even when native per-session metadata had a name |

### Summary

GitHub Copilot CLI stores its session-list name in each session's
`session-state/<id>/workspace.yaml` file. Rosetta discovers that file as the
`session-workspace` stream. The previous adapter left that stream
inventory-only, so both metadata and full event normalization produced
`title: None`.

This is not a missing native title, a bounded-window miss, or an ingest-bench
rendering defect. The adapter never reads the title-bearing field. Unlike Codex,
Copilot has a small, session-local metadata document that provides the native
display name directly; Rosetta does not need to derive one from prompt text or
query a global database.

### Confirmed case

Native session:

```text
79e2bd43-58b9-43ef-8e18-664897f8f0a9
```

The session's native `workspace.yaml` contains:

```yaml
id: 79e2bd43-58b9-43ef-8e18-664897f8f0a9
name: Codebase Overview Session
user_named: false
summary_count: 0
```

The first persisted user message is `give me an overview of this codebase`.
The distinct value `Codebase Overview Session` demonstrates that Copilot has
already generated and persisted a display name; reproducing the name from the
first prompt would be a lossy Rosetta-side approximation.

The derived `~/.copilot/session-store.db` corroborates the same value:

```text
sessions.id      = 79e2bd43-58b9-43ef-8e18-664897f8f0a9
sessions.summary = Codebase Overview Session
```

That database must not be the authoritative ingest source. Copilot describes it
as a rebuildable `/chronicle reindex` projection, and Rosetta's ground-truth
layout already marks it for exclusion from parsers. The per-session
`workspace.yaml` file is the durable source to ingest. The corresponding
`sidebar-sessions-state/*.json` file contains only the cwd and an array of
session ids; it does not contain a title.

The Copilot-only meta run on 2026-08-28 confirmed the projection failure:

| Projection fact | Value |
|---|---:|
| Files matched | 11 |
| Files skimmed | 1 (`events.jsonl`) |
| Files classified inventory-only | 10 |
| Sessions discovered | 1 |
| Sessions with a projected title | 0 |
| Sessions with a projected summary | 0 |

The projected row for the native id above had both `title` and `summary` set to
SQL `NULL`, while its native `workspace.yaml.name` was non-empty.

This title surface is also present in all four checked-in Copilot 1.0.80
fixtures (`basic-chat`, `resume`, `tool-use`, and `tool-denied`). Those files
consistently carry `name`, `user_named`, and `summary_count`, so the confirmed
organic session is not an isolated layout variant.

### Previous code path

1. `groundtruth/copilot/layout.toml` declares `session-workspace` at
   `session-state/*/workspace.yaml` and documents `name` as per-session
   workspace metadata.
2. `CopilotAdapter::stream_driver` enables only the `events` stream as
   `JsonLines`.
3. The adapter test `only_events_has_a_json_lines_driver` explicitly requires
   `session-workspace` to remain inventory-only.
4. `CopilotAdapter::skim` returns immediately for inventory-only streams. For
   the event stream it extracts session identity, ancestry, cwd, version, and
   time, but explicitly returns `title: None` and `summary: None`.
5. During full ingest, the `session.start` normalizer constructs the canonical
   `SessionRow` with `title: None` and `summary: None`.
6. The observed `events.jsonl` contains no persisted title-change event that
   could repair the row later. No Copilot operation promotes
   `workspace.yaml.name` into the canonical session.

Consequently, full streaming and replay cannot repair the null produced by meta
ingest. Increasing the event head/tail windows cannot help because the title is
stored in a different file.

Relevant implementation points:

- `groundtruth/copilot/layout.toml` — declares the title-bearing
  `session-workspace` stream and the derived `session-store` stream.
- `crates/adapters/copilot/src/adapter.rs` — event-only stream driver and null
  meta title.
- `crates/adapters/copilot/src/normalize.rs` — streamed `session.start` also
  emits a null title.
- `groundtruth/copilot/fixtures/1.0.80/*/copilot-home/session-state/*/workspace.yaml`
  — existing native metadata examples.
- `crates/store/src/apply/rows.rs` — session metadata merge behavior that must
  preserve the workspace title across source ordering and replay.

### Implemented resolution

- `session-workspace` is now a `DocumentSnapshot` source with a Copilot-specific
  64 KiB limit. It admits only the flat, format-defined YAML mapping and emits
  no timeline record; `events.jsonl` remains timeline truth.
- The parser captures native id, cwd, name and timestamps. The directory id is
  the fallback and a body/path mismatch is rejected as an explicit unidentified
  compatibility failure rather than attaching the wrong title.
- Workspace and event facts derive one canonical id. A title-less
  `session.start` cannot erase the workspace name; rewrite-in-place admission
  updates generated or user-assigned names, and malformed partial YAML leaves
  the last committed metadata intact.
- `session-store.db` and sidebar state remain inventory/corroboration only. The
  adapter never derives a title from the first prompt.

Unit and end-to-end regressions cover exact generated/explicit names, event
merging, incremental rename, identity mismatch, partial writes and the document
cap. The Copilot-only real-source smoke now skims two files rather than one and
projects native session `79e2bd43-58b9-43ef-8e18-664897f8f0a9` with the exact
title `Codebase Overview Session`; the derived session store is not consulted.

## INGEST-004 — Resolved: Antigravity title bootstrap missed checkpoint objectives

| Field | Value |
|---|---|
| Status | Resolved |
| Recorded | 2026-08-28 |
| Agent | `antigravity` 1.1.21 (with 1.1.20 fixture evidence) |
| Affected paths | Meta ingest, full streaming, and replay |
| Previous impact | All observed Antigravity sessions had `session.title = NULL` despite persisted title-like native metadata |

### Summary

Antigravity exposes two native metadata surfaces relevant to a session-list
label:

- `conversation_summaries.db` has explicit `title` and `preview` columns, but
  the index is sparse and observed rows commonly have an empty `title`;
- early `CHECKPOINT` transcript records contain a generated
  `# USER Objective:` value that is concise and title-like.

Rosetta reads the summary database and correctly keeps `title` and `preview`
separate, but it relies on the sparse database for canonical titles. Transcript
skim unconditionally returns no title, while full normalization retains the
checkpoint only as a summary message. It never promotes the generated user
objective into `session.title`.

This produces the same “No title captured” symptom as the Copilot defect, but
the storage semantics differ. Copilot has a complete per-session `name` field.
Antigravity requires deterministic reconciliation between an incomplete global
summary index and title-like metadata embedded in each session transcript.

### Confirmed corpus

The Antigravity-only meta run on 2026-08-28 reported:

| Projection fact | Value |
|---|---:|
| Files matched | 66 |
| Files skimmed | 4 |
| Files classified inventory-only | 62 |
| Sessions discovered | 4 |
| Sessions with a projected title | 0 |
| Sessions with a projected summary | 1 |

Three current CLI sessions each contain exactly one early checkpoint with a
generated objective:

| Native session | Checkpoint location | `USER Objective` | Projected title |
|---|---:|---|---|
| `3d4449c2-023a-41bf-9c73-5ef10ffab9b6` | line 2, byte 592 | `Codebase Architecture Overview` | `NULL` |
| `66a950e5-4e41-4bae-8ef6-fd950d76eb85` | line 4, byte 867 | `Instruction For System Response` | `NULL` |
| `bcbcd076-faaa-45c0-97f5-c58432794dba` | line 5, byte 1,411 | `Read And Process File` | `NULL` |

All three objectives occur well inside the default 64 KiB head window. Their
loss is therefore not a skim-boundary problem: the adapter does not inspect
checkpoint content during skim.

The fourth projected session,
`e49169a5-2db1-4830-b378-2ec42fb76aa1`, is the only live row in
`conversation_summaries.db`. Its native values are:

```text
title   = ""
preview = "Redesign Terminal Playground"
```

Rosetta correctly projects the preview into `session.summary`, leaving
`session.title` null. `cache/conversation_metadata.json` corroborates the same
empty `Title` and non-empty `Preview`; it does not supply a missing title.

### Summary-index lifecycle evidence

`conversation_summaries.db` is not a complete cold-bootstrap catalog in the
observed CLI lifecycle:

| Corpus | Conversation transcripts | Summary rows | Non-empty titles |
|---|---:|---:|---:|
| Live 1.1.21 root | 3 current CLI transcripts, plus 1 indexed older session | 1 | 0 |
| 1.1.20 `basic-chat` fixture | 1 | 0 | 0 |
| 1.1.20 `tool-use` fixture | 1 | 0 | 0 |
| 1.1.20 `resume` fixture | 1 | 1 | 0 |

The resume fixture's single row has a populated preview but an empty title.
Therefore the database's explicit `title` remains the highest-confidence native
title when present, but the database cannot be the only source used to title
all transcript sessions.

### Pre-fix code path

1. `groundtruth/antigravity/layout.toml` declares the transcript and the
   SQLite `conversation-summaries` stream.
2. `AntigravityAdapter::stream_driver` drives `transcript` as `JsonLines` and
   `conversation-summaries` as `SqliteSnapshot`.
3. `SUMMARIES_SQL` maps non-empty `conversation_summaries.title` to canonical
   `title` and non-empty `preview` to canonical `summary`.
4. Since the live database contains only one of four sessions and that row's
   title is empty, the snapshot contributes no title to any observed session.
5. `AntigravityAdapter::skim` derives identity and the opening timestamp from
   the transcript, but explicitly returns `title: None` and `summary: None`.
6. Full normalization emits `CHECKPOINT` as `MessageKind::Summary` with its
   complete native content. It does not emit a session-metadata update from the
   `# USER Objective:` section.
7. Full streaming and replay therefore preserve the null title even though they
   parse the record containing the generated objective.

Relevant implementation points:

- `groundtruth/antigravity/layout.toml` — transcript and summary-index
  lifecycles.
- `groundtruth/antigravity/sources/1.1.20.sqlite-schema.sql` — native summary
  columns.
- `crates/adapters/antigravity/src/adapter.rs` — sparse snapshot query and null
  transcript skim title.
- `crates/adapters/antigravity/src/normalize.rs` — checkpoint-to-summary
  normalization without title reconciliation.
- `crates/adapters/antigravity/src/raw/transcript.rs` — native checkpoint
  record shape.

### Resolution requirements

#### 1. Preserve explicit native titles

Continue treating a non-empty `conversation_summaries.title` as the highest
precedence title. A later transcript fallback must not overwrite an explicit
native title, and a title-less summary snapshot must not regress an existing
canonical title to `NULL`.

#### 2. Recover generated objectives from checkpoints

When a complete `CHECKPOINT` contains the native generated section:

```text
# USER Objective:
<non-empty objective>
```

extract the bounded objective and use it as the generated session-title
fallback. The parser must recognize section boundaries rather than copy the
entire checkpoint, reject blank/malformed objectives, normalize only format
whitespace, and impose a documented Unicode-safe display bound.

Meta ingest should search complete checkpoint records within its bounded
windows. Full normalization must perform the same extraction and emit an
idempotent session-title update, so complete streaming repairs any title missed
during bounded skim. If multiple checkpoints occur, the precedence between
their objectives must be explicit; the current recommendation is latest valid
objective wins unless native lifecycle research proves the first objective is
the stable display identity.

Some valid checkpoints, including the checked-in 1.1.20 `tool-use` fixture, do
not contain a `USER Objective` section. They must remain untitled unless another
native title source exists; Rosetta must not mistake `User Requests`, tool
summaries, or the entire checkpoint for the objective.

#### 3. Keep preview semantically distinct

`conversation_summaries.preview` should remain `session.summary`. A consumer
may display summary as a visual fallback when title is absent, but the adapter
should not silently relabel arbitrary preview text as a native title. The
observed older preview is title-like, whereas the resume fixture's preview is a
full assistant response; this variation requires a product-level display policy
rather than an unconditional adapter cast.

### Regression coverage

Add minimized fixtures and prove:

1. A non-empty summary-database title remains the highest-precedence title.
2. A transcript missing from the summary database receives its checkpoint
   objective during meta ingest.
3. The same objective is promoted during full ingest when meta skim missed it.
4. Summary snapshot and transcript processing order cannot regress or duplicate
   the session.
5. Multiple checkpoints follow the documented winner rule across incremental
   ingest and replay.
6. A checkpoint without `USER Objective` remains honestly untitled.
7. Blank, multiline, Unicode, oversized, and malformed objectives are handled
   within a declared bound.
8. `User Requests` and other checkpoint headings are never misidentified as the
   title.
9. Preview remains summary data and is not promoted implicitly.
10. Fresh ingest and ledger replay converge on identical title and summary
    values.

### Acceptance criteria

- The three confirmed current sessions project the exact objectives listed
  above as generated titles.
- A populated `conversation_summaries.title` wins over checkpoint-derived
  metadata.
- The sparse or absent summary database cannot prevent a transcript session
  from receiving its generated objective.
- Full ingest repairs a title missed during bounded meta ingest.
- The older `Redesign Terminal Playground` preview remains available as summary
  without being falsely represented as an explicit native title.
- Replay and incremental ingestion preserve the same deterministic result.

### Implemented resolution

- Canonical metadata now carries a ranked `SessionTitle` fact. `explicit`
  native display names outrank `generated` names, which outrank derived
  `fallback` labels. Same-confidence updates replace in deterministic ingest
  order, so native renames continue to work. Streamed `custom_title`,
  `ai_title`, and `title_fallback` summary rows use the same 3/2/1 ordering.
- Antigravity recognizes only the exact `# USER Objective:` checkpoint
  section, stops at the next Markdown heading, rejects blank or malformed
  sections, normalizes format whitespace, and caps display output at 80 Unicode
  scalar values including an ellipsis. `User Requests`, logs, and the complete
  checkpoint body are never promoted.
- Meta skim evaluates complete records in the head and tail and selects the
  valid objective with the greatest native `step_index`. If none is visible,
  Antigravity opts into the shared 128 KiB / 256-record complete-record scan;
  full streaming emits a source-owned `title_fallback` singleton so an
  objective beyond every metadata window still repairs the session.
- A non-empty `conversation_summaries.title` is an `explicit` candidate and
  therefore cannot be overwritten by any checkpoint fallback. `preview`
  remains `session.summary`, including the observed `Redesign Terminal
  Playground` value.
- Antigravity's normalizer version is now 4. Canonical-op goldens cover
  objective-bearing and objective-free checkpoints; unit tests cover section
  boundaries, blank/malformed input, multiline whitespace, Unicode truncation,
  and false-heading rejection. The pipeline regression covers a title between
  skim windows, disabled-fallback stream repair, multiple checkpoints, an
  explicit database override, incremental append, and ledger-only replay.

The 2026-08-28 read-only local metadata smoke now projects the three exact
observed objectives as fallback titles and keeps the fourth session's preview
as summary-only:

| Native session | Projected title | Projected summary | Rank |
|---|---|---|---:|
| `3d4449c2-023a-41bf-9c73-5ef10ffab9b6` | `Codebase Architecture Overview` | `NULL` | 1 |
| `66a950e5-4e41-4bae-8ef6-fd950d76eb85` | `Instruction For System Response` | `NULL` | 1 |
| `bcbcd076-faaa-45c0-97f5-c58432794dba` | `Read And Process File` | `NULL` | 1 |
| `e49169a5-2db1-4830-b378-2ec42fb76aa1` | `NULL` | `Redesign Terminal Playground` | 0 |

The run again matched 66 files, skimmed four, retained 62 as inventory, and
reported zero unidentified files. Its objectives were already inside the
ordinary head windows, so the measured fallback scan remained zero bytes; the
synthetic boundary regression is the proof for that bounded branch.
