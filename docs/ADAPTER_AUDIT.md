# Adapter gap audit

Status: implemented and verified on 2026-08-28.

This audit follows a session through every seam that can make it disappear:

1. layout discovery;
2. physical-source admission and virtual-session enumeration;
3. permissive raw parsing and schema-drift detection;
4. canonical projection;
5. project and parent/root assignment;
6. storage and generation reconciliation;
7. client filtering and rendering.

## Why the app appeared to support only three agents

The live schema-5 projection already contained all nine implemented runtimes. The
problem was not one missing adapter:

| runtime | sessions | no project | child sessions |
|---|---:|---:|---:|
| Antigravity | 3 | 3 | 0 |
| Claude Code | 5,912 | 12 | 4,888 |
| Codex | 381 | 0 | 160 |
| Copilot | 1 | 0 | 0 |
| Grok | 131 | 131 | 0 |
| Hermes | 1 | 0 | 0 |
| Kimi Code | 4 | 0 | 0 |
| OpenCode | 1 | 0 | 0 |
| Pi | 1 | 0 | 0 |
| **total** | **6,435** | **146** | **5,048** |

Three presentation rules hid the evidence:

- the project ledger had no bucket for `project_id IS NULL`, hiding every
  Antigravity/Grok session in this projection;
- the session list retained only roots without embedding their 5,048 descendant
  subagent/team sessions in the root transcript;
- the badge component discarded runtime kinds without a bundled brand mark, making
  six implemented agents visually absent even when they contributed to a project.

The app now has a synthetic, client-only **Unassigned sessions** project. Only root
threads appear in browse/search navigation; descendants are lazy, recursively embedded
inside their canonical parent transcript, and a child search hit opens that root then
expands to the actual descendant message. Proven native spawn links place a child under
the delegate tool; parent-only evidence is interleaved by the child's start time while
remaining visibly unanchored, so approximate reading order is not presented as a proven
spawn edge. The app also renders a deterministic initials badge for every unknown/unbundled
runtime kind. Thinking, system, and summary rows are readable transcript asides instead of
being discarded. “Unassigned” means exactly “the native records admitted so far contain no
usable workspace fact”; it is not an agent classification or an ingest failure.

A schema-6 metadata smoke against the same local roots used a temporary database and
left the live daemon untouched. It matched 39,322 files with zero unidentified files
and admitted every implemented runtime; a Copilot-only rerun after its final workspace
closure confirmed that the title companion adds a second skim without changing the
session count:

| runtime | sessions after metadata closure | no project | child sessions |
|---|---:|---:|---:|
| Antigravity | 4 | 3 | 0 |
| Claude Code | 5,912 | 13 | 4,888 |
| Codex | 382 | 0 | 160 |
| Copilot | 1 | 0 | 0 |
| Grok | 136 | 0 | 16 |
| Hermes | 1 | 0 | 0 |
| Kimi Code | 4 | 0 | 0 |
| OpenCode | 1 | 0 | 0 |
| Pi | 1 | 0 | 0 |
| **total** | **6,442** | **16** | **5,064** |

The remaining 16 sessions contain no usable native cwd and are visible under
Unassigned by design. The smoke was metadata-only; stream semantics are covered by
fixture, split/resume, generation and replay tests rather than by mutating the live
projection.

## Adapter-by-adapter closure

| adapter | authoritative driven sources | gaps closed | deliberate evidence boundary |
|---|---|---|---|
| Antigravity CLI | transcript JSONL + metadata-only conversation-summary SQLite snapshot | admits summary-only sessions; explicit summary-index title, preview, cwd, parent/root and role; bounded checkpoint-objective fallback with stream/replay convergence; recognizes current `GENERIC` records | Antigravity IDE is a separate Tier-D surface; implicit protobuf/maintenance streams remain inventory |
| Claude Code | root + subagent transcript JSONL | current organic type census; bounded complete-record title fallback; stream-level generated/custom title reconciliation; declared metadata variants retain schema-addition drift checks; child sessions embed under their parent delegate when the result carries `agentId` | closed writer: rare teams/compaction records remain version-scoped organic evidence |
| Codex | rollout JSONL + metadata-only `state_*.sqlite` snapshot | complete 0.150.1 writer vocabulary and tool/event projection; full current title bootstrap with generated-name precedence, 36-character Unicode-safe prompt fallback, legacy optional-column compatibility and incremental updates | an organic compaction replay fixture is still desirable; sparse `session_index.jsonl` is corroborating rather than required bootstrap state |
| Copilot | events JSONL + bounded metadata-only `workspace.yaml` document snapshot | content-bearing 1.0.80 events; native generated/user title and cwd/timestamp admission; rename updates; body/path identity guard; all other declared events remain schema checked | the writer declares 53 event types while hermetic fixtures exercise a smaller hosted-model-independent subset; derived `session-store.db` remains excluded |
| Grok | updates JSONL + metadata-only `summary.json` document snapshot | summary-only session admission; title, summary, cwd, start time and parent metadata; current update normalization remains single-rail to avoid duplicate chat-history/events | feedback/checkpoint bodies and some lifecycle variants have no primary captured body evidence |
| Hermes | `state.db` SQLite snapshot | native parent/root/role; exact replacement-keyed session/model usage through canonical `session_usage` rather than fabricated messages | gateway routing, async delegation and compaction state are schema-only |
| Kimi Code | agent wire JSONL + session-state document snapshot | root and nested-agent enumeration; all durable 0.38.0 fixture plus organic record types are typed; config cwd, compaction summaries and todo snapshots project canonically; approval/lifecycle bookkeeping remains typed and drift checked | several organic variants still lack a pinned writer export or hermetic fixture |
| OpenCode | current `opencode.db` SQLite snapshot | current 1.18.23 writer tables and message/part vocabulary; reasoning, patch, retry and full tool status/result/usage projection; parent/root topology | pre-1.2 JSON stores and external tool-output bodies remain research-only |
| Pi | session JSONL | fractional provider cost is accepted losslessly; existing message, thinking, tool, usage, compaction and ancestry semantics remain intact | extension-defined custom/label records have no stable cross-agent canonical meaning |

Cursor is not counted as an adapter gap. Its ground-truth pack is explicitly blocked:
the closed CLI offers no endpoint override and no credentialed capture lane is
provisioned. Inventing a parser from the IDE database or reverse engineering would
violate the evidence rules.

## Cross-cutting fixes

### Rewrite-in-place documents are first-class sources

`StreamDriver::DocumentSnapshot` now handles one bounded JSON/text document that enumerates
zero or more virtual sessions. It is used by Grok summaries, Kimi state documents and
Copilot workspace metadata; adapters may lower the generic document cap (Copilot uses
64 KiB).
Each virtual session gets its own cursor/generation and can emit its own metadata and
records without treating the physical file as one fake session.

Membership is authoritative. Rewriting a document/database emits tombstones for removed
members. Deleting the physical document/database retains its virtual cursors long enough
to run an empty generation for every member, so stale messages, metadata and search rows
are swept. Live hints and polling both detect whole-source deletion.

### Project identity is stable and privacy-safe

Workspace roots are lexically normalized and are the default project identity, so two
ordinary clones of one remote remain two projects. During meta ingest the shared adapter
contract may prove either a local checkout-group root or ownership by another native
session. The generic resolver uses that evidence to merge linked worktrees, including
deleted temporary worktrees whose agent metadata survives. When no native ownership is
available, the resolver reads Git's local common-directory relation without invoking Git;
the sanitized remote is display metadata only. A missing cwd remains null and is presented
honestly as Unassigned.

The project-evidence pass was repeated across every implemented adapter on 2026-09-01.
All nine use the shared live-filesystem resolver; only an adapter fact that distinguishes a
linked worktree from an ordinary clone is allowed to outlive that filesystem evidence:

| adapter | bounded native evidence | project decision |
|---|---|---|
| Antigravity CLI | first `workspace_uris` entry supplies cwd; `project_id` does not prove a local checkout group | cwd + live Git common directory |
| Claude Code | transcript cwd; `agent-*.meta.json` optionally carries an absolute `worktreePath`, while its enclosing session directory identifies the parent | an explicitly isolated child uses `SameAsSession(parent)`; every other session uses cwd + live Git |
| Codex | rollout/state cwd, parent thread and remote/branch metadata; no persisted Git common-directory or worktree-owner fact | cwd + live Git; remote identity never groups clones |
| Copilot | `workspace.yaml` cwd and event repository context; no native worktree-group marker | cwd + live Git |
| Grok | subagent metadata persists child cwd plus parent session identity | child uses `SameAsSession(parent)`; ordinary sessions use cwd + live Git |
| Hermes | session cwd and `git_repo_root`; the latter describes the current checkout/worktree, not its shared common Git directory | cwd + live Git |
| Kimi Code | one session-state cwd shared by the root and its enumerated agents | shared cwd + live Git |
| OpenCode | session directory plus a project row whose id may be remote/root-commit based and whose `sandboxes` can include another clone | ignore native project grouping; use directory + live Git so ordinary clones stay distinct |
| Pi | session-header cwd and optional parent-session path; no worktree-group marker | cwd + live Git |

This boundary is intentional: Claude Code and Grok can still join a deleted temporary
worktree to its surviving owner. For the other adapters, a deleted checkout remains separate
unless a future, ground-truth-backed writer field proves its original Git common directory.

### Native session usage does not manufacture messages

Projection schema 6 adds `session_usage`, keyed by `(session_id, sample_key)`. It stores
replacement-keyed usage totals that a native agent reports for a session/model/task but
cannot attribute to one response. Replacements apply exact bucket corrections and remain
idempotent. Hermes is the first producer.

### Titles converge across metadata and streams

An adapter may request a separately measured, bounded scan of complete records when the
ordinary head/tail skim lacks an optional fact. Claude uses a 128 KiB / 256-record cap;
its full stream remains the convergence path if the title lies deeper. Source-owned
`ai_title` and `custom_title` rows refine a retained metadata fallback, with explicit
titles winning independently of order, batches, replay or generation replacement.

Title candidates now carry evidence confidence rather than relying on which file happened
to be scanned last: explicit native labels rank above agent-generated labels, which rank
above prompt/checkpoint-derived fallbacks. Antigravity uses that lowest tier only for the
exact bounded `# USER Objective:` checkpoint section. Full streaming emits the same
source-owned fallback, while a populated `conversation_summaries.title` remains explicit
and its semantically distinct `preview` remains summary data. The read-only local rerun
projected all three observed checkpoint objectives exactly and left the fourth preview-only
session honestly untitled.

Codex and Copilot titles come from their native metadata companions instead of prompt
guessing. The real-source smoke projected 348 of 382 Codex sessions with titles (maximum
36 characters), recovered the known Claude boundary title, and projected Copilot's exact
`Codebase Overview Session` label.

### Declared no-ops are not schema blind spots

Bookkeeping or duplicate rails may intentionally emit no canonical row, but their raw
shape is still validated. New fields produce aggregated drift; undeclared variants are
soft drift; malformed known variants are hard drift. This distinction is now explicit
for Claude Code, Copilot and Kimi in addition to the adapters whose complete variants
already use typed structs.

## Verification

The closure is guarded by:

- package-level fixture replay, lossless round trips, golden canonical ops and
  split/resume tests for all nine adapters;
- document/SQLite membership rewrite, member removal, physical deletion and live-hint
  integration tests;
- the daemon adapter matrix, which freezes JSONL, document-snapshot and SQLite-snapshot
  contracts separately;
- exact `session_usage` replacement/rollup tests;
- `cargo test --workspace`, strict Clippy, ground-truth regeneration checks, and the app
  TypeScript production build.

Deploying these changes requires an explicit projection rebuild because the store schema
is now version 6 and several normalizer versions changed. After rebuild, run source ingest
once so newly driven metadata documents/databases are admitted; do not reuse a schema-5
projection as evidence of the new behavior.
