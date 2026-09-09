# Rosetta Architecture

Rosetta is a multi-agent data ingestion engine, the ground-up rewrite of `p008/spaghetti`.
It has exactly three jobs, in priority order of its consumers:

1. **Search** — full-text search over agent transcript content.
2. **Observe** — full agent metadata + transcripts with live updates, for the agent-view app.
3. **Monitor** — live distilled metric feeds (token activity, tool usage, lifecycle) for
   runtime monitors like `p008/chopsticks`.

The production runtime is Rust end to end; development-time evidence tooling is
standard-library Python. Speed is a feature: cold backfill of a full `~/.claude` in
seconds, file-change → consumer latency in the low tens of milliseconds.

> **Implementation checkpoint (2026-08-31).** Nine ground-truth/runtime-adapter packs are
> registered. The generic runtime handles adapter-selected JSONL, bounded document snapshots,
> and SQLite snapshots; virtual membership/deletion, generation reconciliation, deterministic
> checkpoints, polling/audits, and the checksummed raw ledger are implemented. Replacement
> generations and late tool/usage relations stage only in process RAM. A process exit leaves the
> prior projection active and reconstructs the lost accumulator by raw-ledger replay; no normalized
> operation or reconciliation payload is serialized into SQLite. Unknown projection schemas fail
> closed and are replaced only by an explicit rebuild. When a new version admits native metadata
> that an older ledger never captured, one fresh source ingest is required before future ledger-only
> rebuilds become complete again.
>
> Projection schema 14 keeps the one canonical SQLite projection separate from a disposable SQLite
> FTS5 sidecar. `projection_state`, the coalesced `search_dirty` repair set, and their asynchronous
> worker landed together. Full generations publish through an atomic `CURRENT` manifest; epoch,
> revision, acknowledgement, corruption, and crash tests prevent cross-generation results.
> Canonical lists/timelines remain available while search is building. Tantivy 0.26.1 remains an
> optional benchmark-only alternative; SQLite remains the canonical transactional store.
> Verified inline images are immutable, content-addressed members of that canonical projection.
> SQLite carries relocatable service URLs, not Base64; the lossless ledger remains the replay
> source.
>
> **Transcript work-index checkpoint (2026-09-03).** Schema 14 records only source-proven task
> start/terminal transitions and adds a partial `(session_id, ordinal)` index containing only task,
> plan, and artifact messages.
> The Rust `timeline_meta` read composes the complete task board, unique Workflow-run summaries
> with exact current and journaled worker-session references,
> latest openable plans, and deduplicated openable artifacts from those canonical facts in one
> session-scoped boundary. Resumed Workflow attempts collapse to their canonical run id and retain
> the newest attempt as a transcript anchor; the rich run document is not copied into this index.
> The protocol-v14 initial timeline snapshot carries that metadata beside the recent message window;
> relevant commits coalesce to one replacement `timeline_meta_snapshot`. Plan/file bodies remain
> lazy, and an exact session-scoped timeline-row read lands old tasks or Workflow attempts without
> paging through the intervening transcript. This is a read model only: no second
> task/workflow/plan/artifact
> representation is persisted, and the existing messages remain the transcript record.
>
> **Known task-namespace defect (2026-09-04).** Claude's `TaskCreate`, `TaskUpdate`,
> `TaskList`, and `TaskGet` describe shared work items, but `TaskOutput` and `TaskStop`
> address heterogeneous background execution handles. An execution may be a local
> subagent, an in-process Agent Team teammate, or a background shell; it is not thereby
> a task-board item or a workflow. The current Claude adapter classifies the latter two
> tools as `ToolAction::Task`, and the `timeline_meta` query consequently admits stop
> handles as empty task rows. The required boundary is fail-closed: only an explicit
> work-item payload or an existing canonical task row may enter `timeline_meta.tasks`.
> Execution output/stop belongs to process, delegation, or team lifecycle (or bounded
> unknown when its kind is unproven), and cannot synthesize task status or timing. A
> session containing only execution-control events has no Tasks launcher or Tasks tab.
> Session ownership is closed during bounded metadata admission: one normalized primary cwd and
> one optional local-checkout project link per session. Schema 13 uses folder identity by default,
> closes native checkout-group/session-owner hints, and merges only linked worktrees. The stream
> phase cannot manufacture projects or reassign ownership; schema 12 removed the former
> `workspace_obs` relation and `project_assigned_at` arbitration column.
>
> **Fresh-serve checkpoint (2026-08-31).** The normal first-run app path now uses the fast
> parallel cold topology without giving up immediate or progressive reads. The listener binds
> before discovery. Each adapter's bounded source-free metadata commits first to a named,
> shared-memory SQLite database carrying the exact canonical schema and drains into the Hub;
> projects and top-level sessions therefore appear adapter by adapter. All adapters then capture,
> ledger, normalize, and reconcile concurrently in private RAM planners. The sole canonical
> writer admits results in completion order, but only at a closed-runtime boundary: precomputed
> ordinals and cross-file tool/usage folds are complete before the final readiness commit makes
> that runtime's timelines visible. Pending sessions are an admission barrier, so intermediate
> source commits cannot leak a half-reconciled canonical row.
> Watched timelines receive one resync barrier and re-snapshot through the ordinary API. No raw
> record, parser state, candidate value, or reconciliation map enters the read target.
>
> After every runtime closes, set-based indexes/rollups, `ANALYZE`, quick/FK checks, and projection
> validation run against that RAM generation. The named `<ledger>.staged` raw generation is then drained and fsynced;
> the canonical database is backed up to a same-filesystem private file, synced, installed by
> atomic rename, and the read router switches RAM → disk before the RAM keeper is released.
> Detached search stays explicitly Building and its worker remains paused until this switch.
> Watchers were armed before metadata, so source suffixes written during the build feed the live
> runner after handoff. RAM is a temporary location for the one canonical projection, not a third
> representation or a recovery source. The app keeps one HTTP/WS contract across the switch.
> Canonical payload visibility is runtime-granular; exposing a partially reconciled multi-file runtime
> for cosmetically finer progress is forbidden. A future session-partitioned plan must first prove
> that every correlation and replacement boundary is closed inside the partition.
>
> **Cold progress (2026-09-07, protocol v16).** Observation is independent of publication.
> Metadata's atomic session count advances with each admitted identity, before ownership settlement
> publishes the adapter's complete metadata batch. A `ColdProgressObserver` reports framed payload
> at bounded record-batch boundaries; `PayloadBytes` excludes audit, anchor, and prefix rereads.
> Fresh and resumed serve populate a shared RAM `ProgressBoard` from metadata's settled ownership.
> `/api/status` snapshots that board while the writer is busy, and a project's `sessions_done`
> advances only after the complete runtime becomes public. Metadata-only parent placeholders do
> not overcount the source plan. Database/document payloads and resumed suffixes have an unknown
> byte denominator (`bytes_total: 0`); serialized records must never be divided by SQLite file size.
> JSONL byte totals remain source-size estimates, with UI clamping for delimiters and growing files.
> Recovery explicitly reports `restoring`; runtimes distinguish `preparing` and `applying` after
> input reading, and the final canonical phases distinguish `finalizing` from `publishing`.
>
> Detached search reports `SearchState.progress` for its unpublished generation: document counts
> during `indexing`, then `optimizing`, `validating`, and `publishing`. The denominator comes from
> the same canonical snapshot, and its atomic numerator advances every 256 documents. Progress is
> scoped to the projection epoch, reset for retries, and cleared after publication; it cannot leak
> an old full-build observation into ordinary live search repair. These observations persist
> nothing and never change either canonical or search readiness.
>
> The renderer polls status every 500 ms until canonical **and** search readiness are satisfied,
> then returns to 30-second polling (intentional consent/audit pauses use 2.5 seconds). Requests
> are serial, abort after five seconds, retain the last observed progress on failure, and retry
> after one second. A compact panel above the project list describes four user-facing steps:
> discovery, reading, preparation/saving, and search. Step-local meters use real denominators;
> opaque preparation/validation work stays indeterminate. Throughput uses an eight-second window
> of observations, expires after four seconds without movement, and resets on phase changes,
> restarts, or regressing counters. Longer counter pauses distinguish an answering service from
> unchanging work counters. Errors explicitly pause the indicator instead of resetting it.
> Available projects and sessions remain selectable, aggregate figures have a separate
> `statsReady` gate, and completion briefly confirms success before the panel closes. Stage-only
> polite announcements, native disclosure controls, tabular figures, and reduced-motion styles
> keep frequent updates readable without repeatedly announcing changing counts.
>
> **Progress validation.** Campaign `/private/tmp/rosetta-ingest-progress-20260907` retains three
> before/after fresh-serve pairs on the same read-only 200-file, 316,975,655-byte Claude/Codex
> corpus (source manifest SHA-256 `f7c85b319eeb1779a0e29f3dba39bc6be29a221cda09a082109c37b3a3b2fb93`)
> and 12-query ranked-result suite (`e8f727b1d2e12b99e25a145665019c67b5e0eb9c3d0575b64b8acfe51b89d65d`).
> All 17 normalized canonical table digests and all 120,151 search documents/ranked results match
> across six runs. With identical 50 ms diagnostic polling, the new board exposes 40–71 distinct
> project folds and search exposes 52–82 document counts per run; the baseline exposes neither.
> At the renderer's 2 Hz cadence, the short fixture yields 7–10 project observations. Baseline
> byte numerators regress at runtime completion; the new payload numerator has zero regressions.
> Median user CPU is 11.634 → 11.618 s, system CPU 1.968 → 1.993 s, and peak RSS 1,590.3 → 1,600.2
> MiB on the same M1 Max/64 GiB Mac. Wall times vary with shared-machine I/O (median full readiness
> 15.863 → 13.396 s), so this is evidence of low observation overhead, not a throughput speedup
> claim. Binary hashes, commands, full status traces, digests, and timings live in `campaign.json`,
> `serve-results.json`, `progress-analysis.json`, and `verification.json` in that campaign.
>
> The progressive read contract is intentionally asymmetric. Project and top-level-session lists,
> bounded session lookups, and timelines may read the canonical shared-memory generation. Global
> heatmap, usage, and contribution endpoints return HTTP 503 with protocol-v7 `not_ready` until the
> reader switches to the published file; search stays explicitly Building. This is a correctness
> and scheduling boundary, not a loading effect: an earlier renderer launched two aggregate queries
> for each of 135 projects every five seconds, and those shared-cache readers starved the sole cold
> writer. After publication the renderer hydrates project figures with four workers and a quiet
> refresh cadence. Contribution SQL constrains the project session tree before aggregating instead
> of re-grouping the whole corpus for every project.
>
> The app also owns first-run recovery and binary selection. `pnpm dev` builds release `rosettad`
> and passes that exact path to Electron. Electron starts plain `serve`. Before any payload capture,
> a fsynced `.cold-generation.json` inside `<ledger>.staged` binds the projection/ledger names,
> enrollment, and runtime roots. An exit discards only canonical RAM. The next launch reconstructs
> it from sealed staged-ledger generations, treats a raw-only tail as unfinished, reserves that
> generation in RAM, and rereads the source strictly above it. A crash after ledger promotion but
> before SQLite installation remains resumable because the marker moves with the ledger; successful
> SQLite installation removes it last. `--restart-incomplete` remains an explicit legacy
> archive/restart command, not app startup policy. Recovery never treats RAM as durable state.
>
> A complete projection no longer re-enters the following disk-progressive scheduler. It is marked
> Live before serving, arms watchers before a bounded current-inventory skim, then performs one
> non-blocking queued-hint drain plus forced cursor-relative stat poll. Only changed/new sources
> stream, and the same effect drain feeds HTTP/WebSocket subscribers. The disk-progressive path
> remains for legacy incomplete projection resume and `--stop-after-meta`, but is superseded for a normal fresh app launch. Its listener
> and public projection exist first, consent
> precedes all reads, and every runtime is skimmed before streaming. Cold streaming then runs
> through a four-lane scheduler (live ▸ boost ▸ starter ▸ completion, round-robin across runtimes
> inside the cold lanes). Watcher hints drain into the live lane DURING the backfill, but only to
> reprioritize work inside the metadata cut; once that source's cold task has been consumed, later
> growth belongs to the live runner and cannot recreate a cold obligation. This termination rule
> lets canonical publication complete while agents keep writing. A claimed physical path no
> roster entry covers still re-runs bounded meta so a genuinely new source appears immediately.
> Virtual members of one document/database coalesce into one physical snapshot pass. The 500 ms mirror
> rewarm poll is gone: metadata effects (identity keys only, collected on every path including
> cold generations) are drained by the writer thread between passes into `Hub::absorb_meta`,
> fanning precise `/ws/meta` diffs with no polling, no second read connection pinning the cold
> WAL, and no hub mutation off the writer thread. Row effects run SCOPED during cold: each pass
> arms the store with `Hub::watched_sessions` (the open `/ws/session` subscriptions), so an open
> transcript receives its prioritized cold rows — appends push precise rows; a cold seal, retry sweep,
> abandon, or append renumber pushes one `SessionSwept` marker and the subscriber re-snapshots —
> while unwatched sessions pay no mutation log at all (their metadata still flows), and the live
> handoff widens the scope to every session. The cold phase itself is one named unit,
> `rosettad::engine::ColdEngine`: it settles an `IngestPlan` (projects → sessions → sources with
> census bytes, from the skims' committed ownership; unproven ownership stays honestly unassigned;
> a multi-session source is ONE planned pass) and folds pass outcomes through a `ProgressBoard`
> onto `/api/status` as per-project rows (`ingest.projects`, protocol v7) — sessions done/total
> and bytes streamed/total per project, refreshed between passes and kept current through
> mid-cold admissions. Plan, board, and schedule are RAM only, rebuilt from a skim on every run;
> durable progress remains cursors, generations, and `stream_state`. Starter AND completion passes carry a byte-governed slice budget (`--slice-budget`,
> default 1 MiB with a 24-record floor — the measured knee of the 2026-08-31 slice-sweep campaign
> (frozen 9-runtime snapshot, 5 repeats, 32k–2m grid: starter drain ~3–5 s at every budget, full
> cold drain ≈35 s at 256 KiB vs ≈19 s at 1 MiB against an unsliced ≈13 s floor, with 2 MiB
> trading starter latency for little further gain); batches are soft-capped to the slice so a slice is
> ~one transaction): a fresh source's clean prefix ACTIVATES and seals its ledger generation at
> the boundary — indistinguishable from the file ending there and then growing — so every session
> opens readable early while paused remainders requeue as ordinary append growth; app boosts run
> unbudgeted and jump both lanes, preempted by at most one slice, and a session badges Indexed
> only when its last queued task is gone. Two bounds are explicit: snapshot-driver sources
> (SQLite/document) always stream whole because their revision identity is a whole-content hash,
> and a source that turns unclean inside a pass falls back to whole-file handling. Dirty content
> follows live-growth semantics on every cold path (decided 2026-08-31): a dirty FIRST generation
> activates its parseable rows and the session holds Streaming, never Indexed — identical sliced,
> unsliced, or rebuilt, with rebuild electing latest seals for only-dirty paths — while a dirty
> REPLACEMENT still abandons to protect the active clean generation, and oversized or
> raw-incomplete sources still abandon. An idle daemon appends NOTHING to the raw ledger
> (fixed 2026-08-31, found by visual QA): the rotating audit re-reads a quiet file against the
> tail checkpoint's committed content chain (checkpoint v2 — a resumable byte fold over
> `0..committed`; v1 blobs still decode and the first audit establishes their chain in place) and
> escalates to a replacement replay only on a real mismatch — the old audit forced that replay
> unconditionally, re-appending every audited file to the append-only ledger each rotation and
> sweeping its projection. Snapshot databases stat-gate: an unchanged stamp is never even opened
> (a read-only SQLite open writes `-shm` beside the source, the watcher saw the daemon's own
> footsteps, and the resulting hint loop starved the cold lanes — `-shm` is never a hint, and
> poll admissions need evidence: first sighting or movement, never presence). Passes carrying an
> audit budget still hash snapshot content whole, so a stamp-preserving rewrite is caught by the
> audit sweep. A final per-runtime audit pass, `milestone:` stdout lines
> for the budget-sweep harness (search-ready is claimed only when the detached build actually
> reached Ready), and the graceful-stop boundary close the cold phase. Interrupted commits resume
> in place. Finite `ingest` and `rebuild` stay private, validated, atomically installed, and never
> slice. Watchers are armed before scan; the initial search build gets a clean I/O window, then
> the live lane reuses the existing roster rather than re-skimming it.
>
> The accepted six-adapter frozen pair below measured that historical predecessor disk-progressive
> path, not the current normal fresh-app path. It
> used 38,998 matched files, 6,063 skims, 6,419 sessions,
> and 774,219 final messages with zero hard errors. Private atomic wall was 522.54 s. Progressive
> milestones were listener 1.19 s, lists 5.82 s, first readable 13.86 s, canonical ready 538.11 s,
> search ready 704.74 s, and total wall 704.82 s (1.349× private; gate 1.5×). Full SQLite/FK checks,
> exact normalized content-table digests, sidecar cardinality, and 100-cycle ranked-result digests
> matched. The canonical file was 4.691 GB and its detached index 3.430 GB. Diagnostic FTS5
> broad-ranked p95 was 271–380 ms versus selective-ranked p95 26.5–35.4 ms; this supports continued
> Tantivy evaluation but not adoption without relevance and product-mix evidence. Private paths,
> fingerprints, and query text remain unrecorded by policy.
>
> A 2026-08-31 visual product-path diagnostic on the current nine-runtime organic corpus found the
> renderer/shared-cache contention above: the same release/RAM path reached canonical readiness in
> 225.02 s before read isolation. After the gate, bounded hydration, and project-first contribution
> query, metadata-to-canonical took 52.36 s; combining that with the earlier 9.35 s automatic metadata
> phase gives a 61.71 s diagnostic cold-work estimate. The direct visual run was 77.64 s because it
> included manual consent. The final projection passed quick/FK checks with 134 projects, 6,553
> sessions, and 569,004 messages. These are organic measurements, not a frozen before/after pair and
> therefore not merge-admissible performance evidence; they establish the reproduced contention
> mechanism and visual/integrity gate only. Detailed measurements live in
> `docs/COLD_INGEST_IO_FLOOR.md`.

---

## 1. Lessons from spaghetti (what we keep, what we kill)

The autopsy of spaghetti (~151k LOC Rust NAPI crate + ~99k LOC TS + ~15.5k LOC Python)
directly shapes this design.

**Kill:**

| Spaghetti disease | Rosetta cure |
|---|---|
| Three coexisting data models in one 101-table schema (legacy flat, provenance plane, canonical plane) | Exactly two representations: an append-only **raw ledger** and one **canonical projection**. Nothing else, ever. |
| Duplicated parsers in Rust and TS (~20k LOC legacy TS kept as "differential oracle") | Parsers exist **once**, in Rust adapter crates. TS consumers get **generated types** from the Rust source of truth. |
| No migrations — schema version mismatch silently drops the whole DB | The DB is a **disposable projection by design**: on projection-version bump, rebuild from the ledger at disk speed. Losing the DB loses nothing. |
| `projection.rs` at 14,102 lines | Normalization lives in per-agent adapter crates; per-domain modules; CI check caps file size. |
| Consumer contract pinned by string (`spaghetti-sdk@0.7.0-local+rfc012cd`, `link:` dep) — breaks silently | A versioned wire **protocol** with a handshake, semver'd generated client package, and golden contract tests. |
| Schema drift only quarantined into `unknown_native_evidence`, invisible | Drift is a first-class fact: captured per record, surfaced in a `rosetta probe` report and a drift table. |
| Path resolution split between TS (`paths.ts`) and Rust | Agent-specific source knowledge stays in the paired `groundtruth/<agent>` data pack and `crates/adapters/<agent>` parser; the declarative layout is canonicalized once. |
| No network server — NAPI + custom `SPAG` MessagePort framing couples every consumer into the host process | One daemon, localhost HTTP/WS. Any process, any language, no build matrix. |

**Keep (spaghetti's good bones):**

- Watch-before-scan ingestion (subscribe first, then scan — no gaps), ~20 ms event
  coalescing, periodic audit sweep as a safety net.
- Per-object opaque cursors + transactional commits (its `source_objects.committed_cursor`
  / `ingest_commits` pattern was right).
- FTS5 **external-content** tables (index without duplicating text).
- The `observeSession` semantics — epoch / sequence / resync. Chopsticks already speaks
  this shape; rosetta's observe protocol keeps the same concepts so porting is a
  transport swap, not a rewrite.
- Content hashing with blake3, JSONL parsing with sonic-rs.

**The most important recon fact:** chopsticks consumes exactly **one** spaghetti API —
`observeSession`, a store-free live event stream. The 34 query methods and the SQLite
file have no external consumer. Rosetta's service surface is therefore small and
consumer-driven from day one.

---

## 2. The three layers

```
┌─────────────────────────────────────────────────────────────────────┐
│  L3  SERVICE PROVIDER                          crates: protocol,    │
│      REST /api/search, /api/sessions …                server       │
│      WS   /ws/sessions/{id}, /ws/meta  (resumable)                  │
│      WS   dedicated metrics feed                   (target)         │
├─────────────────────────────────────────────────────────────────────┤
│  L2  ADAPTERS + NORMALIZER (the runtime)       crates: core,        │
│      capture: watcher/tailers → raw ledger             capture,     │
│      per-agent parsers (raw types) designed            adapter-*,   │
│        AGAINST the common interface, with the          pipeline,    │
│        L1 ground-truth file as their spec              store        │
│      normalize ──pure fn──▶ canonical facts + analyzers             │
├─────────────────────────────────────────────────────────────────────┤
│  L1  GROUND TRUTH (dev-time evidence pipeline) data: groundtruth/   │
│      fixture farm · mock model · census · schema merge              │
│      layout manifests · facet extractors        tools/groundtruth/  │
│      watch.toml release tracking                                    │
│      PRODUCT: canonical JSON + generated HTML per captured agent    │
└─────────────────────────────────────────────────────────────────────┘
         ▲                    ▲                     ▲
     ~/.claude            ~/.codex               ~/.grok
```

Layer 1 is pure ground truth: a dev-time evidence pipeline whose finished product
is ONE canonical `groundtruth.json` plus its generated `groundtruth.html` report
per captured agent (iron rule #6). It runs no production code. Layer 2 is the
entire runtime: capture, per-agent parsers
written against the canonical model with the L1 file as their specification
(never against fixtures or intuition), normalization, analyzers, storage.
Data flows strictly upward. Consumers never reach past L3 into the store; L3
never parses agent files; L1 never knows what a "session view" is.

---

## 3. Layer 1 — Ground truth evidence

**Sourcing principle:** ground truth is derived from reproducible, authoritative sources
— agent source code, official release channels, and hermetically generated fixtures —
never from whatever agent data happens to sit on a developer's machine. Local data may
corroborate; it is never load-bearing. The per-agent sourcing strategy, fixture-farm
design, and release-watch automation live in
[groundtruth/RESEARCH_PLAN.md](groundtruth/RESEARCH_PLAN.md).

### 3.1 One data pack and one parser crate per agent

`groundtruth/claude-code`, `groundtruth/codex`, `groundtruth/grok` hold evidence only.
Production parsers live separately under `crates/adapters/<agent>`. The pair is the
single home for everything Rosetta knows about an agent:

```
groundtruth/claude-code/
├── watch.toml
├── layout.toml
├── fixtures/<version>/
├── census/
├── sources/
├── groundtruth.json
└── groundtruth.html

crates/adapters/claude-code/
├── src/raw/
├── tests/fixtures_replay.rs
└── Cargo.toml

tools/groundtruth/
├── farm.py
├── mock_model.py
├── census.py
├── model.py
└── render.py
```

### 3.2 The layout manifest

A declarative description of the agent's on-disk footprint — the answer to "what files
are stored in the agent folder":

```toml
format = "rosetta-layout/1"
agent = "claude-code"
data_root = "~/.claude"

[[streams]]
id = "transcript"
glob = "projects/*/*.jsonl"
file_format = "jsonl"
discipline = "append_only"
evidence = "fixture"
```

`layout.toml` is an authoring intermediate. The evidence pipeline folds it into
`groundtruth.json`; runtime parser tests read the canonical JSON, never the TOML
directly, so there is still only one authoritative truth surface.

Spaghetti tracked 16 Claude streams; the manifest is where that inventory lives, is
diffed, and is version-annotated. Any file the watcher sees that matches **no** stream
becomes an `UnknownFile` drift fact — new agent features announce themselves.

### 3.3 Versioned raw schemas

The per-file schemas — "for each file, what is its data schema, tracked by agent
version release":

- Raw types are **serde structs, permissive by policy**: every struct carries
  `#[serde(flatten)] unknown: Map<String, Value>`. A new field in an agent release
  never breaks parsing — it lands in `unknown` and emits a `DriftObserved` fact with
  the field path and a sample. (`deny_unknown_fields` is banned: it turns every agent
  release into an outage.)
- A declared record may intentionally be a canonical no-op because it is bookkeeping or
  duplicates an authoritative rail. That does **not** exempt it from schema checks:
  typed variants retain flatten-unknown paths, while declared raw fallbacks compare their
  keys with the embedded canonical shape. Undeclared variants are soft drift; malformed
  known variants are hard drift.
- Where formats self-stamp, version detection is **per record** — Claude Code transcript
  lines carry a `version` field; Codex rollout files stamp theirs. Where they don't,
  `detect.rs` falls back to structural heuristics. Detected version is recorded on every
  raw record.
- Variant shapes across versions are modeled as enums (`RawTranscriptLine` with
  version-gated variants), not parallel copies of whole modules.
- `groundtruth.json` is **THE canonical ground truth — one file per agent, the
  only source of truth** (`rosetta-groundtruth/1`, implemented by
  `tools/groundtruth/model.py`): stream layout, every record type with its
  strongest evidence class, every field's shape/requiredness/observation count.
  It is rebuilt by `build_canonical` from the intermediate captures under
  `groundtruth/<agent>/sources/` (fixture-derived docs per version, census-derived docs
  per date, SQLite schema dumps) — intermediates contribute, never rule.
  `tools/groundtruth/render.py` writes the committed sibling
  `groundtruth.html`; the JSON diff across a release regeneration is the drift
  review artifact and the HTML is its browsable form.
- `fixtures/<agent-version>/` holds per-version sample corpora, produced by the fixture
  farm (hermetic scripted runs against pinned agent versions — see RESEARCH_PLAN.md),
  redacted and determinized. Every fixture is replayed in CI: parse → normalize →
  golden-file compare. Adding support for a new agent version = regenerate fixtures,
  watch what breaks, extend types, commit new schema export.

### 3.4 Drift operations: `rosetta probe`

A subcommand that scans the live agent directories and reports, without ingesting:
unmatched files, unknown fields per stream (with counts and sample paths), parse
failures, and newly observed agent versions. The same detection runs continuously during
normal ingestion, landing in a `drift_events` table. This is the standing answer to
"keep up with each agent's updates": run probe after an agent updates, refresh fixtures,
extend the raw types.

### 3.5 Runtime handoff: watcher, tailer, and raw ledger

These mechanisms consume the canonical L1 layout but belong to the production
runtime in `crates/capture`; they are described here only to make the evidence →
capture contract explicit.

- **Watcher** (`notify`/kqueue): subscribe first, then initial scan (watch-before-scan),
  with bounded hint coalescing and periodic recovery polls/audits. Every `serve` and
  `watch` runtime shares one coalesced `SourceWake`; the writer waits at most 250 ms
  once per quiet pass, then calls each runner's non-blocking `tick_ready`. An exhausted
  hint budget requests another fair pass immediately. Serve publishes committed effects
  after each runner, before visiting the next. Per-runtime stat polls remain on their
  30-second cadence; a dead watcher requests one rescan and then falls back to polling.
- **Tailer**: per-file cursor = byte offset + line count + hash of a head prefix.
  The head hash detects rewrite/truncation (Claude Code rewrites files on compaction);
  on mismatch, the file re-ingests under a new **epoch**. Partial trailing lines are
  buffered, never half-parsed.
- **Bounded metadata fallback**: the ordinary 64 KiB head + 16 KiB tail skim remains
  the default. An adapter may opt a source into a separately metered scan of complete
  records when one optional fact is still missing; the generic 128 KiB / 256-record
  ceilings remain owned by the pipeline. Claude Code uses this only for a missing main-
  session title; Antigravity uses it only when no complete checkpoint in the ordinary
  windows exposes the exact `USER Objective` section. Complete streaming remains the
  eventual convergence path for both.
- **Snapshot drivers**: `SqliteSnapshot` opens a consistent native database view;
  `DocumentSnapshot` reads one stable, bounded rewrite-in-place JSON, YAML, Markdown, or
  text document. Adapters may lower the pipeline document limit for a small native format
  (Copilot caps `workspace.yaml` at 64 KiB). Both drivers may enumerate multiple virtual
  sessions from one physical source. Membership is authoritative:
  rewrites tombstone removed members, and deletion of the whole physical source runs an
  empty generation for every retained virtual cursor before removing stale projection rows.
- **Title reconciliation**: bounded metadata emits a ranked `SessionTitle` fact instead
  of relying on file-walk order. Explicit native labels outrank generated labels, which
  outrank prompt/checkpoint-derived fallbacks; same-rank replacements preserve native
  rename behavior. Streamed title singleton rows use the same precedence and fall back
  to the retained metadata answer after a source-generation sweep.
- **Workspace settlement**: every adapter selects one primary cwd from its session header,
  metadata document, or native session table. Child sessions may inherit a proven parent cwd
  when their own metadata omits it. The meta phase normalizes that path, resolves its repository
  root without invoking Git, journals the resulting `SessionRow` and `ProjectRow`, and only then
  admits the source for streaming. Missing evidence remains `NULL`; transcript cwd changes are
  never a fallback ownership channel.
- **Raw ledger**: every complete raw record is appended with source/generation metadata
  to size-rotated segment files, checksummed with blake3, and durably flushed before the
  corresponding projection transaction. By default `rosettad` stores one runtime
  ledger at `<projection>.ledger/<agent>/`; `--ledger` overrides that root. Segments are
  self-describing and scanned incrementally during rebuild. Since 2026-09-04 a new segment
  lands inside one zstd stream (magic `RLZ1`); the frame stream inside is byte-for-byte the
  plain `RLG1` one, per-frame blake3 integrity is unchanged, and readers keep both containers
  forever, so nothing is rebuilt for the format's sake. The stream flushes at every checkpoint
  — a zstd block ends, the frame does not — so the durable prefix stays decodable while the
  match window survives; rotation and close write the epilogue, and a torn or corrupt tail
  reports exactly as a plain one would. Level 3 measured on a real 9.65 GB archive: 5.56×
  overall (claude-code 5.0×, codex 5.6×, grok 8.6×) at 1.0–1.6 GB/s compress and 1.9–3.1 GB/s
  decompress on one core, all on the ledger worker thread that already owns segment IO;
  compressing frames individually would halve the ratio, and lz4 lost on both axes.
  `rosettad ledger compact` rewrites older plain segments in place under the projection lock
  (temp file, sync, decode-and-digest verification, rename; torn segments are left alone).
  Its `--level` stops at 19 and `--long` is explicit because a zstd window is a read-side
  cost as much as a write-side one: every later decoder of a segment allocates its window
  (2 MiB at level 3, 8 MiB at 19, 128 MiB for `--long` or levels 20–22) for as long as the
  segment is open, and a replay opens one per runtime.
  Each closed segment also appends one line to `<agent>/.segments` naming the sources and
  generation ranges it holds, so a read scoped to one source generation (the transcript's
  exact-command fetch) opens the owning segments instead of walking the ledger; the index is
  derived and rebuildable, and it only ever narrows a read — an unlisted segment is always a
  candidate. Two reasons this exists:
  1. **Agents garbage-collect their own data** (Claude Code's `cleanupPeriodDays`
     deletes old transcripts). Without a ledger, history — and your FTS index over it —
     silently rots.
  2. It is the **replay substrate**: re-running an improved normalizer over all history
     is a ledger scan, not a prayer that source files still exist.

---

## 4. Layer 2 — Normalizer

### 4.1 One canonical model, consumer-driven

> **Status: working hypothesis.** The model below is NOT frozen until the Phase-1
> ground-truth survey covers the full agent roster and the Phase-2 pattern synthesis
> validates these shapes against every agent's reality (§10). Spaghetti's canonical
> model was designed from Claude Code alone and every later adapter had to contort to
> fit it — rosetta does not repeat that.
>
> The current detailed hypothesis — eleven entities, the flattened message-row taxonomy,
> the discover/meta/stream ingest phasing, and the concrete SQLite DDL, checked and amended
> against 9 of 11 ground-truth packs but not yet ratified — lives in
> [`docs/canonical-model.html`](canonical-model.html) (rendered design doc, 2026-08-29).
> Where that file and this section disagree, the HTML is newer.

The canonical model is deliberately small — it covers what the three jobs need, nothing
more:

```rust
enum CanonicalFact {
    SessionUpserted(Session),     // agent kind, primary cwd/project, native id, times, status
    EntryAppended(Entry),         // typed user/agent/reasoning/tool/notice/summary body,
                                  //   parent/causal-group/response linkage, timestamps
    ArtifactUpserted(Artifact),   // todos, plans, tasks, team state
    MetricEmitted(MetricSample),  // e.g. Usage { input, output, cache_creation, cache_read }
                                  //   keyed (session, response_key), canonical flag
    LifecycleEvent(Lifecycle),    // session start/end, compaction, subagent spawn/finish
    DriftObserved(Drift),
}
```

### 4.1.1 Strict public message boundary

**Internal session exclusion (2026-09-09, schema 18).** An adapter may mark an
identified source `ledger_only` during bounded metadata normalization. Capture
still records its complete native bytes, but metadata admits no session, project,
or title for it and stream normalization emits no canonical operations. Its nullable
`ingest_cursor.session_id` retains only resumable capture progress; no dummy session,
message, asset, usage rollup, or search repair key is created. The admission bit lives
in the raw ledger's source definition so a fresh-ledger replay cannot recreate a
placeholder. Existing source definitions default to ordinary projection. Codex's
internal guardian approval reviews use this policy; ordinary delegated work and
code-review subagents remain visible. This is a fresh-ingest boundary, not a cleanup
or migration of an existing archive.

> **Activation status (2026-08-30): shipped in projection schema 12.** Core types, every captured
> adapter, canonical store/search, protocol, and the client share one closed user-facing timeline
> and fixed tool vocabulary. Native input/result/payload objects remain ledger-only. Correlation,
> evidence ranking, progressive snapshots, and late joins are normalization mechanics, not public
> fields or durable SQLite facts.

A privacy-safe point-in-time audit of the nine installed runtimes classified 377,877 tool
calls. The draft native mapping covered 65.46% overall and 99.65% after excluding Codex's
opaque outer `exec` envelope. Claude, Codex, and Grok carried 2.79 GiB of native tool-result
values, validating aggressive reduction, while ID-less Antigravity calls, progressive Hermes
and Claude snapshots, missing turn/response boundaries, and request fields available only in
later records disproved the draft's two-phase/native-id assumptions. These organic measurements
corroborate the design; refreshed canonical ground truth and hermetic fixtures remain the
activation authority.

The raw ledger is the complete source record. A canonical timeline is instead a
user-facing semantic projection. These are deliberately different jobs:

- `user` means human-authored prose, not merely a native `role: "user"`; tool results,
  developer prompts, environment injections, and transport wrappers cannot become user
  messages.
- `agent` and `summary` contain human-readable prose, never serialized JSON. Reasoning is
  retained only when it is non-empty, plain, and user-visible. Redacted, encrypted,
  signature-only, and empty thinking remains ledger-only; response usage/model/timing facts
  reattach instead of disappearing with a dropped wrapper.
- Lifecycle records become a closed `Notice` union (compaction, truncation, model/mode change,
  permission request/decision, handoff, local-command output, runtime error), not arbitrary system
  text. Adapter normalizer versions invalidate old projections when native transport wrappers gain
  a new semantic projection. Generated/custom titles refine `session.title`, not the timeline.
- Attachments are typed image/document/audio/video/file references, while citations remain
  distinct bounded source/span references. Inline bytes become immutable content-addressed
  projection members; the database retains relocatable refs. Attachment-only prose rows are
  valid and do not fabricate empty text.
- Every entry belongs to a deterministic causal group classified as a human turn, agent cycle,
  parent invocation, system cycle, or synthetic group. Group and optional response derivation
  (`native`, `parent_linked`, `ordered`, or `synthetic`) prevents clients from presenting an
  inferred boundary as exact source evidence.

A tool opener, progressive snapshots, and terminal update fold into one `ToolInteraction`
with a deterministic adapter correlation key, optional native id, namespace/name/provider,
evidence-backed status/timing/failure, and one closed semantic action. There are no
universal `input`, `result`, or `payload` fields. The action families are:

`file_read`, `file_list`, `file_search`, `code_lookup`, `file_change`, `shell`,
`code_execute`, `process`, `orchestration`, `web_search`, `web_navigate`, `web_fetch`,
`delegate`, `task`, `plan`, `collaborate`, `question`, `skill`, `workflow`, `schedule`,
`artifact`, `tool_discovery`, `mode`, `finding`, `notification`, `structured_output`,
`external`, and `unknown`.

Live, progressive, corrected, and out-of-order records use closed request/outcome patches
parallel to this union while normalizing. They have no text/JSON/generic variant and wait only
in bounded process RAM when their opener has not arrived. Missing fields never clear proven
facts. Evidence rank plus source order resolves corrections deterministically; duplicate
snapshots are idempotent. Only the resolved `ToolInteraction` crosses into SQLite. The store
rejects a known-family change, while an `unknown` row may upgrade once when later request
evidence establishes the registry-selected family.

Each family keeps only the data necessary to answer its user-facing question. In
particular: reads keep path/range but not file content; file changes keep path plus exactly
one canonical diff when provable but not whole old/new files, and leave create-versus-update
absent when the source cannot prove it; successful shell calls keep command/cwd/
status but no stdout or stderr; web fetches keep URL/status but no page body. A failed,
denied, interrupted, or timed-out shell call may keep a typed failure and at most 4 KiB / 40
lines of deterministic diagnostic text. User-critical large text (prose, commands, diffs,
collaboration bodies, and structured answers) becomes a content-addressed member rather
than being silently truncated. Members are verified and durably renamed before a
referencing canonical row commits; the detached search worker resolves their full text only
after that commit. They remain part of the disposable canonical projection, not a third
source of truth.

Native parsers stay permissive and drift-aware. An adapter-owned semantic projector may
narrow native values into the common union, but narrowing failure **never** fails the raw
record. Unknown extra fields never erase proven semantic fields. A known typed request
survives an unfamiliar completion: reliable status/timing may still merge, the new outcome
is omitted, and drift is recorded. Unprovable optional facts remain absent. Only an
unregistered namespace/name or an unclassifiable record emits `unknown`. Either case records one aggregated
drift shape plus bounded ledger provenance and stores none of the rejected object. Replaying
the unchanged ledger after adding a projector recovers the richer row.
Registered MCP/plugin projectors may emit at most bounded scalar display fields and resource
references; arbitrary nested JSON cannot cross the boundary.

The projector registry is versioned adapter-owned Rust code, keyed by agent, writer-version
range, namespace, and native name. Each entry has typed request/update functions plus explicit
status, search, and bounds policies. It is neither runtime JSON configuration nor another
ground-truth file. In particular, Codex `exec` becomes an honest orchestration action when no
executed-child trace exists; static JavaScript references can never fabricate child actions.

The detached search projection accepts only five semantic lanes: `prose`, `target` (path,
URL, recipient, resource), `intent` (command, query, objective, subject), `change`
(canonical diff), and low-weight `tool` (canonical kind plus native namespace/name for unknown
actions). A typed exhaustive extractor produces these lanes; the worker never traverses stored
JSON. Read bodies, successful shell output, fetched pages, success
acknowledgements, injected prompts/environment, opaque external values, and empty thinking
are neither stored nor indexed.

**Coverage is prompts and replies (product decision, 2026-09-09).** Only `user` and `agent`
rows become search documents — their prose plus attachment names and citation titles/sources.
Tool activity, thinking, summaries, collaboration traffic, and notices remain canonical and
render in the transcript but are not indexed; the extractor keeps its lanes for them so the
policy is one predicate (`rosetta_store::queries::SEARCH_KINDS`) shared by the document
visitor, the build denominator, and the legacy in-projection path, and the sidecar schema
(4) forces an index built under the broader coverage to rebuild. Measured 2026-09-09 on a
clonefile copy of the organic nine-runtime projection: 584,002 message rows become 151,705 search
documents, the published generation shrinks from 1,043,517,440 to 340,185,088 bytes, and the
forced rebuild reports `building` with live document progress from the first second and reaches
`ready` 35 s after daemon start (indexing ≈13 s, then optimize/validate/publish). Protocol clients receive
generated discriminated unions and render by their tags; key sniffing and best-effort JSON
parsing are forbidden.

The exact per-family keep/drop/search rules, nine-adapter native-name mapping, shared bounds,
target DDL, and activation tests live in
[`docs/canonical-model.html`](canonical-model.html#canonical-message-boundary), “The
canonical message boundary.”

IDs are deterministic: `SessionId = (agent_kind, native_session_id)`,
`EntryId = blake3(session_id, native_uuid_or_line_hash)`. Same input → same IDs, always —
that's what makes replay and dedupe trivial.

The `MetricSample` usage shape intentionally mirrors what chopsticks already reads from
`UsageRevisionV2Fact.buckets` (`input_tokens`, `output_tokens`,
`cache_creation_input_tokens`, `cache_read_input_tokens`).

### 4.2 Normalization is a pure, versioned function

```rust
fn normalize(&self, raw: &RawRecord) -> Vec<CanonicalFact>   // no IO, no clock, no rand
```

Each adapter stamps a `normalizer_version`. Determinism + the ledger buys the core
doctrine:

> **The canonical store is a cache.** Bump the normalizer or the projection schema →
> rebuild derived state from the ledger. There are no migrations because there is
> nothing to migrate — the ledger is the truth, the DB is a materialization.
> (Spaghetti "drops the DB on mismatch" as a failure mode; rosetta does it as a
> designed, lossless operation.)

Differential replay is also the safety net for normalizer changes: run old and new
versions over the same ledger, diff the canonical output. That is what spaghetti kept
20k LOC of legacy TypeScript around to do — here it's a subcommand.

### 4.3 Analyzers (distilled data)

Derived/calculated data (token activity time series, per-session/per-day rollups, tool
usage stats, activity heatmaps) are **incremental fold functions over canonical facts**,
computed inside the same commit as ingestion (they're cheap) and stored in rollup tables
stamped with an `analyzer_version`. Same disposability rule applies: rollups rebuild
from canonical facts, canonical facts rebuild from the ledger.

---

## 5. Layer 3 — Service provider

### 5.1 Transport decision: one daemon, localhost HTTP/WS

`rosettad` is a single daemon owning the store (single writer). Consumers connect over
`127.0.0.1` (axum: REST for queries, WebSocket for streams) — optionally a Unix domain
socket for the latency-obsessed. Why not NAPI like spaghetti: multiple consumer apps
(agent-view, chopsticks, future TUIs) shouldn't compile a native addon into every host
process, and localhost round-trips are microseconds — the speed argument never favored
in-process coupling enough to pay its build-matrix and lifecycle-coupling costs.

Escape hatch for bulk analytics: the SQLite file is WAL-mode and read-only-openable by
tools (sqlite-viewer style), explicitly documented as *unstable internal format*.

### 5.2 The three services

**Search (job 1):**
`GET /api/search?q=&agent=&runtime=&project=&session=&after_ts=&before_ts=&kind=&is_error=` → FTS5 match
with snippets and rank, with structured filters applied inside the ranked query. `q` uses the
backend-neutral product grammar: whitespace-separated required terms plus double-quoted required
phrases (4,096 bytes and 64 clauses maximum). Engine operators and field selectors are ordinary
text and never cross the wire. Any explicit time bound excludes rows whose timestamp was never
reported; unknown is not coerced to epoch zero. Unsupported kinds (anything but `user` and
`agent`, the indexed kinds), a set `is_error` (tool status is not indexed), blank selectors,
oversized selectors, and inverted ranges are HTTP 400 errors rather than successful empty
searches.
Each hit carries its row's canonical `kind` and reported timestamp beside the snippet
(protocol v17, 2026-09-09), read from the detached sidecar's own document table — so the
app's search sheet can say who said it and when without a second read per hit.

Protocol v18 (2026-09-09) adds the live-monitor shape of a search, driven by Godview's
"find the agent by what was said" sheet, which names the sessions it can jump to and
asks one question per keystroke: `sessions=` is a comma-separated set of at most 256
canonical ids matched as one `IN` scope (the server widens each id to every session that
rolls up to it, so a hit inside a delegated child answers for its root; an alternative to
`session`, never combined); `prefix=true` widens each term of three or more characters to
the tokens it begins while phrases stay exact (`puppet` finds `puppeteer`; the `*` is
appended outside the quoted token, so user text still never reaches FTS5 unquoted); and
`GET /api/sessions?native_session_id=a,b` resolves provider-native ids to canonical rows,
children included, without listing history. A sidecar generation that is merely behind the
canonical watermark now answers with its consistent snapshot labelled `catching-up`
instead of an empty page; only another projection epoch, an unpublished canonical, or a
failed/absent generation is `unavailable` with no hits. Blank selectors and oversized
sets remain HTTP 400.

**Observe (job 2)** — the flagship contract, successor to `observeSession`:

```
WS /ws/sessions/<id>?epoch=<epoch>&seq=<seq>
  server → { frame: "hello", protocol_version: 17, stamp }
  server → { frame: "timeline_snapshot", stamp, entries, timeline_meta, cursor, watermark }
  server → { frame: "mutation", stamp, mutation } …
  server → { frame: "timeline_meta_snapshot", stamp, timeline_meta }
  server → { frame: "resync" }                # gap/epoch change → replacement snapshot
```

On a resumable reconnect, the snapshot is replaced by unstamped replay mutations,
an optional unstamped metadata replacement, then `caught_up { stamp }`. Even an
empty replay ends with this checkpoint. `hello` advertises the replay target; the
client advances its resume stamp only after applying a snapshot, a stamped live
frame, or the complete replay. A disconnect halfway through replay repeats the
unacknowledged gap safely. Registration, replay capture, and its endpoint share
one hub lock. Async subscribers wait on `Notify`, with transport keepalive pings;
no thread or periodic polling bridge is allocated per socket.

The snapshot's cursor fetches the preceding page directly. HTTP timeline pages
and exact-row envelopes carry a generation watermark, read in the same SQLite
transaction as their rows. The TypeScript subscription owns visible rows, held
history, and anchors in one cache, applies live patches to all loaded identities,
and aborts outstanding reads on replacement. A stale watermark requests a fresh
snapshot. Notifications coalesce over 16 ms; ordered metadata caches update only
the changed collection and retain unaffected published arrays.

The contract lives in `rosetta-protocol`; `ts-rs` generates its TypeScript types.
Both HTTP's exposed `x-rosetta-protocol-version` header and WebSocket's `hello`
are checked before the client accepts data. Golden tests pin the JSON shapes
and check the Rust and TypeScript version constants. HTTP requests have a 30 s
default deadline and accept caller cancellation; changing the daemon origin
aborts requests and reconnects active subscriptions. Search cancels superseded
requests and ignores their late results.

`GET /api/runtimes`, `/api/projects`, root-only `/api/sessions`, and addressable
`/api/sessions/{id}` serve the metadata/browse side of the agent-view app; `/ws/meta`
streams the complete topology so descendant timelines can be embedded recursively.
`GET /api/sessions/{id}/timeline/{timeline_id}` resolves one exact canonical landing row;
scoped plan and file routes resolve bytes only after an explicit open action.

`WS /ws/collaboration/{id}` carries a complete collaboration-boundary snapshot,
content-free response model/effort observations, then filtered live changes. It
does not send private transcript windows or timeline metadata. Team roster
changes add/remove only the affected subscriptions. Opening a report's work
disclosure calls `GET /api/sessions/{id}/timeline/{timeline_id}/work`; the daemon
resolves the source session, response, and inclusive bounds from that canonical
report and returns only the proven range. Pending or missing evidence fails closed.

Electron owns one managed daemon generation at a time. `serve --json` emits a
versioned readiness event with its bound loopback origin. `serve --announce` additionally
publishes `{schema: "rosetta-daemon/1", origin, pid, protocol_version, db, started_at}` as
`<pid>.json` under `~/Library/Application Support/rosetta/daemons` (macOS) or
`$XDG_STATE_HOME/rosetta/daemons` (`--announce-dir` or `ROSETTA_ANNOUNCE_DIR` override),
owner-readable, removed on orderly shutdown, and named in the ready event as `announce`.
It exists for sibling apps that did not spawn the daemon and therefore never saw its
stdout; a reader treats the file as a hint and probes `/api/status` before believing it,
because a killed daemon leaves its file behind. The app passes `--announce` (2026-09-09). Startup failure and
shutdown send SIGINT, escalating to SIGKILL after a grace period; replacement
waits for actual child exit and retries with capped backoff. Main sends revised
daemon state through the preload so the renderer reconnects when a new child
binds another port. Quit waits for daemon exit. Explicit external daemons remain
externally owned. Built renderers load from the registered `rosetta://app` origin,
which shares the daemon's HTTP and WebSocket origin allowlist with development;
opaque `null` origins remain disallowed.

**Metrics (job 3):**
`WS /v1/metrics?families=usage,lifecycle&agent=…` — live distilled feed. Hot path is
memory-first: capture → normalize → `tokio::broadcast` fan-out happens in parallel with
the DB commit, so monitor latency doesn't wait on fsync. Frames carry seq numbers and
the stream is at-least-once; anything display-critical reconciles via a durable REST
read. Slow consumers get coalesced updates + overflow signal rather than back-pressuring
ingestion.

---

## 6. Storage

- **SQLite** (rusqlite, bundled), WAL, `synchronous=NORMAL`, single-writer daemon,
  batched transactions. One live ingest transaction applies resolved canonical rows, rollup
  corrections, minimal cursor/provenance watermarks, and coalesced search repair keys.
  Normalized operations, pending joins, opener copies, evidence winners, and replacement payloads
  stay in RAM. Canonical rows, rollups, joins, and the generation watermark remain atomic; after a
  crash the ledger recreates the discarded accumulator instead of SQLite acting as an event log.
  Ranked session titles follow the same boundary: SQLite retains one selected title and bounded
  winning provenance, never the losing candidate set.
  Session cwd/project ownership is admitted once from bounded adapter metadata. Adapters
  may provide an agent-neutral checkout-group root or a same-project native-session edge;
  the meta resolver closes those edges before journaling. Otherwise it falls back to the
  local Git common directory, which merges linked worktrees but deliberately keeps ordinary
  clones of the same remote separate. Stream normalization may retain a current execution
  cwd in checkpointed adapter state solely to
  resolve relative tool arguments; it cannot write that state to canonical tables, consult today's
  checkout to enrich history, create a project, or reassign a session. Filesystem-derived project
  identity is captured during meta ingest and written into the ledger before it becomes canonical.
  The store rejects `SessionMeta` in every source-scoped stream batch, making this a write-boundary
  invariant rather than an adapter convention.
- **Content-addressed image members** are part of the one canonical projection, not a third
  representation. An adapter may recognize and decode a verified native inline image, but only
  the store writes it to `<projection>.assets/<blake3>.<ext>`. Live ingestion makes the immutable
  file durable before committing its `/api/assets/<blake3>.<ext>` URL. Private builds sync all new
  members before database publication; monotonic content-addressed additions let the final
  SQLite file remain the only atomic swap. The raw ledger keeps the original inline form.
- **A generation this daemon cannot use is no generation.** A well-formed `CURRENT` manifest
  whose search schema or format differs reads as absent (2026-09-09): the worker rebuilds, status
  reports `building` with the rebuild's progress, and a query answers unavailable — a schema
  bump is a routine rebuild, never a `failed` install. After a generation publishes, every other
  regular file in `generations/` (superseded generations, their WAL/SHM, the temp file of a
  build that died) is pruned; the sidecar is disposable and a reader still holding a superseded
  file keeps its inode until it closes.
- **Detached SQLite FTS5 sidecar today**, behind the backend-neutral search/readiness
  seam. A worker builds private immutable generations, atomically selects one with
  `CURRENT`, repairs complete sessions by epoch/revision, and acknowledges only after the
  search commit. Search loss or corruption never blocks canonical reads and triggers a
  rebuild. The disposable Tantivy prototype clears latency/build/filter/determinism gates
  but not relevance, product-mix, or storage acceptance, so it is not production.
- **On-disk layout:** `<projection>.ledger/<agent>/` (`RLZ1`/`RLG1` segments plus the derived
  `.segments` index) + the configured SQLite file +
  optional canonical `<projection>.assets/` members; `--ledger` can relocate the segment root.
  An adopted search backend owns a disposable `<projection>.search/`. Ledger is precious; the
  canonical projection and search index are rebuildable.
- Keep the schema compact rather than chasing a numeric table target. Entities, rollups,
  drift, cursors, generation staging, and the proposed search revision/repair set each
  need a named consumer; a new table without one is not justified.

## 7. Performance playbook

> The detailed implementation design — the pure op-stream pipeline, crate seams,
> stream-phase concurrency, and the bench/profiler infrastructure (bench-corpus
> generator, per-stage criterion benches, `rosettad ingest --profile`, the rebuild
> headline bench) — lives in [`docs/ingest-engine.html`](ingest-engine.html)
> (rendered design doc, 2026-08-27). Where it and this section disagree, the HTML
> is newer.

### 7.1 Cross-adapter applicability checkpoint (2026-08-27)

The store hot path is shared, but the evidence used to optimize it must not be. The
committed canonical-op goldens and restart tests currently establish this matrix:

| adapter | driven source | tool-result merge | usage representation in fixtures | restart proof |
|---|---|---:|---|---|
| Antigravity | 1 JSONL + metadata SQLite snapshot | yes | none observed | split/resume + ranked-title ledger/snapshot replay |
| Claude Code | 2 JSONL streams | yes | inline on messages | split at every fixture line |
| Codex | 1 JSONL + metadata SQLite snapshot | yes | separate `Usage` merge | split/resume + native-title snapshot replay |
| Copilot | 1 JSONL + metadata document snapshot | yes | inline + separate `Usage` merge | split/resume + metadata rename replay |
| Grok | 1 JSONL + metadata document snapshot | yes | separate `Usage` merge | checkpoint/one-shot + document replay |
| Hermes | 1 SQLite snapshot | yes | none observed | stateless snapshot replay equality |
| Kimi Code | 1 JSONL + metadata document snapshot | yes | inline on messages | split/resume + document replay |
| OpenCode | 1 SQLite snapshot | yes | inline on messages | split at every snapshot record |
| Pi | 1 JSONL | yes | inline on messages | split/resume state test |

“None observed” is fixture evidence, not a claim that a future release cannot report
usage. [`adapter_optimization_matrix.rs`](../crates/rosettad/tests/adapter_optimization_matrix.rs)
fails if the roster, stream-driver contract, checkpoint round trip, or canonical op
classification changes. Every adapter has full canonical-op goldens; Hermes' snapshot
projection is frozen alongside the eight existing golden corpora.

The newer frozen-source campaign adds private real-source evidence for Antigravity,
Claude Code, Codex, Grok, Hermes, and Kimi Code. Copilot, OpenCode, and Pi were not
installed and remain fixture/restart evidence rather than real-source performance
evidence. Hermes now supplies a non-empty SQLite snapshot lane, although its 25 messages
are too small to extrapolate snapshot-driver throughput. The six non-empty runtimes
produced 773,375 messages with exact FTS, usage-bucket, tool-call, and token aggregates,
zero generation/topology/ordinal/staging defects, and ranked-search-digest-equivalent
cold and ledger-rebuilt projections. Semantic backlog remains explicit: 550
Claude metadata-only sessions are Pending and 79 result records are unmatched. Seven of
those results have a call only in their parent/root session, so cross-session correlation
is not yet closed. The stream also retained 195,484 unknown-type/no-op outcomes plus
parsed-record unknown-field drift; the newer snapshot retained 195,959 unknown outcomes.
Zero hard errors means preservation succeeded, not that every current native shape has
canonical semantics. This paragraph is the retained pre-closure performance baseline;
the 2026-08-28 semantic audit subsequently closed document metadata admission, current
writer/type vocabularies, session-scoped usage, project identity, and client visibility.
Evidence-blocked and intentionally non-canonical rails are listed in
[`docs/ADAPTER_AUDIT.md`](ADAPTER_AUDIT.md), rather than being implied by a raw unknown count.

This makes the optimization boundary explicit:

- Presentation renumbering is below the adapter boundary. A retained frozen replay
  showed 5,918 per-session window/sort passes consuming 220.410 s. Private first
  generations now keep collision-free provisional ordinals and canonicalize every
  session once at finalization; live and replacement swaps retain transactional
  per-session ranking. On the same ledger this reduced wall from 513.58 s to
  317.49 s (−38.2%) and comparable canonical-plus-ordinal work from 302.092 s to
  88.671 s (−70.6%). Exact logical and ranked-result equivalence passed. Peak RSS
  rose from 190 MB to 502 MB, so partitioning that finalizer is a future measured
  memory/speed tradeoff, not a free follow-up.
- Batch session timestamp touches and RAM-only late-arrival reconciliation are roster-wide:
  all captured adapters exercise the shared message/tool/usage store paths. A batch orders
  openers before updates, folds each tool correlation once, and writes the resolved row once.
  Cross-file orphans wait in process-local maps indexed by relation target and source
  cursor/generation. Openers look up only matching pending fragments; activation and
  abandonment look up only the affected source generations. Ordinary append/stage batches
  never scan unrelated pending state. All index updates happen after the SQLite commit,
  including changes in ownership when a newer usage snapshot replaces an orphan. A restart
  intentionally discards these maps and replays the affected ledger scope.
- Same-batch tool-result folding is roster-wide in semantics, but the measured SQLite
  implementation is rejected. On the frozen mixed ledger it safely folded 358,790 of
  360,052 tool results plus 149,755 of 151,298 separate usage snapshots (99.45% overall)
  and retained exact logical counts. Fully populated message inserts nevertheless raised
  message-write time from 9.327 s to 17.113 s and canonical apply from 36.923 s to
  42.805 s (+15.9%). The candidate was reverted at `0d5796d`; schema 11 instead performs a
  single RAM fold before canonical application without a persisted pending path.
  Inline usage remains part of message application, while separate usage ops currently
  come from Codex, Copilot, and Grok.
- Larger live JSONL tail batches may apply to seven adapters, but the analogous private
  ledger-replay experiment is rejected. Raising the replay boundary from 512 to a
  source-byte-bounded 2,048 ops cut batches from 14,301 to 12,275 and commits to
  3.754 s, while canonical apply regressed from 36.923 s to 48.171 s (+30.5%) and
  peak RSS rose from 511 MB to 673 MB. It was reverted at `93a2ece`. Hermes and
  OpenCode still require a separate SQLite snapshot-query batch/memory experiment;
  no JSONL or ledger-replay result may be projected onto them.
- A generation seal already durably checkpoints activation/abandon, but removing the
  second clean final checkpoint is not a retained performance optimization. Exact
  post-seal dirty tracking skipped 5,918 of 14,301 replay batches while preserving a
  byte-identical `ingest_cursor` table and exact projection totals; commit time still
  measured 9.715 s inside its retained 8.5–11.4 s range. The candidate was reverted at
  `52a533e`. Replay batch count is not a performance proxy on this storage configuration.
- Payload-light ledger election is below the adapter boundary and therefore roster-wide,
  but the RLG1 implementation was rejected. Paired frozen scans saved only 0.19–0.34 s
  (0.9–1.6%) of wall time even though one pair reduced user CPU from 8.66 s to 0.28 s;
  seek-heavy variants regressed. Do not carry the extra two-pass safety surface. Any future
  shortcut needs a writer-produced authenticated control index or footer that the full
  checksummed replay verifies.
- Raw ledger framing and buffering are also below the adapter boundary, and the latest
  frozen-source pair separated them. Borrowing payloads removed the temporary raw-frame
  allocation but moved total ledger time from 18.237 s to 18.394 s, so it was reverted.
  Raising only the writer buffer to 1 MiB reduced buffered write plus explicit flush from
  7.950 s to 6.522 s and total ledger time to 16.988 s; `25d18e9` retains that change.
  Source checksums, projection aggregates/integrity, segment shape, all post-meta ledger
  bytes, and 100-cycle ranked-search results matched. The measured cost is about 5 MiB peak
  RSS across six open runtime writers; the 7.41 s whole-wall delta is not attributed because
  FTS alone varied by 6.406 s.
- Inline-image recognition is adapter-specific; content-addressing, durability, and serving
  are shared projection facilities. The retained `image-assets-2026-08-29` pair used the same
  checksummed, permission-read-only 88,498,056-byte Claude transcript on one macOS/aarch64
  machine. Its 56 image records decoded to 54 unique original PNG/JPEG files (15,489,826 bytes).
  SQLite fell from 43,712,512 to 22,622,208 bytes (-48.2%); SQLite plus image members was
  38,112,034 bytes (-12.8%), because decoded binaries avoid Base64 expansion. `tool_payload`
  fell from 29,220,858 to 8,184,806 bytes (-72.0%). Canonical application moved from 1.074 s
  to 1.017 s (-5.3%) even after 40.3 ms of image materialization; tool-result writes moved
  from 45.7 ms to 19.0 ms. The raw ledger remained 89,294,939 bytes and therefore lossless.
  Every decoded byte and every non-image payload field matched. A frozen 100-cycle product-query
  campaign retained exact hit counts and ranked-result digests; p95 moved about +1.1% in both
  search lanes, which is treated as run noise because detached FTS does not read `tool_payload`.
  This is one payload-heavy file, not evidence for whole-roster wall-time extrapolation.
- Private-cold relational reconciliation is now a generation-scoped RAM fold rather than a
  per-fragment SQLite loop. The `cold-tool-ram-reconcile-2026-08-30` pair used that same
  permission-read-only 88,498,056-byte Claude transcript (SHA-256
  `7ecfbffce129ee8c0fc07e2b0b28325f569227cc32cb1894aa9c67327dc02c96`) on the same
  macOS/aarch64 machine, release builds, and identical CLI/configuration. Baseline `d4abca8`
  versus the final, retry-hardened candidate moved canonical apply from 4.079 s to 0.171 s
  (-95.8%), total store apply from 4.172 s to 0.349 s, and process wall from 5.02 s to
  1.29 s. The candidate spent 26.3 ms folding 2,566 correlations in RAM and 32.1 ms
  inserting 5,132 reduced facts;
  tool-result lookup/write timers fell from 1.799 s/59.6 ms to zero because each final tool
  row was inserted once. One max timestamp touch per affected session reduced touch attempts
  from 8,011 to 2,880. Deferring tool rows moved 3,926 provisional ordinals into the global
  38.1 ms renumber pass, which is included in the wall result. Both sides retained 24,854
  parsed records, 12,557 ops, 3,934 messages, 2,566 tool rows, 5,132 reduced inputs, 868 edits,
  equal usage/aggregate rows, zero logical `EXCEPT` differences after mapping
  surrogate message IDs, and passing quick/foreign-key checks. Frozen search campaign
  `67b44f4c6ec55edea241aaeb6760b725580d8d1c52fae599b2e00b6fbc9a6b2a` used the
  checksummed `product-diagnostic-v2` suite (BLAKE3
  `00dcab42b607869ed696fe70caed9ab16c7ffe3507453e8f3ffc2ebccbc4fbe1`), 5 warmups and
  100 cycles; the comparison passed equal schema/configuration, logical counts, filters, hit
  counts, and ordered-result digests. This optimization applies to every adapter emitting the
  universal `Message::ToolUse` / `ToolUpdate` / response-usage / file-edit vocabulary. The later
  schema-11 storage-boundary change removes the reduced-fact writes too; its performance claim
  requires a new frozen before/after pair rather than reusing this earlier measurement.

The applicability is general; unpaired saving estimates are not. Always-on split counters
attribute replay cost by adapter and canonical statement family. The former target-aware SQLite
pending gate at `8fc0b50` was useful historical evidence (7.905 s to 0.424 s on the frozen
ledger), but schema 11 deletes that SQL path entirely. A new frozen campaign must measure the
RAM-only implementation before any further wall-time claim is merge-admissible.

The schema-11 storage-boundary validation replayed the same permission-read-only 26-segment
Claude ledger twice after the final code change: 1,241,440 records, 5,445 sources, 5,933 sessions,
329,955 messages, 208,124 resolved tool rows, and 41,786 file edits. Final release wall was
95.43 s (the preceding run was 95.90 s), with 24.818 s canonical apply, 9.346 s normalization,
6.645 s global renumbering, 17.959 s detached search build, passing `quick_check`/foreign keys,
and zero private reconciliation objects. The canonical SQLite file fell from the retained
schema-10 artifact's 1,344,462,848 bytes to 666,062,848 bytes (-50.5%); detached search stayed
effectively flat at about 394 MiB. `tool_update_fact` plus its two indexes had occupied about
447 MiB, while removing opener/merge state from `message` cut that table from about 503 MiB to
274 MiB. Public message/tool/title/file-edit/usage rows matched after excluding surrogate ids and
source-provenance keys. The audit also found 6,415 historical `workspace_obs.git_repo` values that
the old live path had inferred from a checkout but never captured in the ledger. Schema 12 removes
that relation entirely: one primary cwd and its meta-settled local checkout identity are captured
in the ledgered meta phase, and stream projection has no ownership mutation op. Peak RSS was 3.64 GB, so partitioning the disposable cold RAM
fold is the next measured memory target. There is no speedup claim here: an exact same-machine
schema-10 rerun is still required before attributing the roughly 95-second absolute wall.

The private-generation insert-first candidate at `207bc37` exercised all 786,004 message
attempts and fell back on 17,724 legitimate repeated timeline identities. Message identity
lookup plus write moved from 22.601 s to 20.859 s (−1.742 s / −7.7%), but total canonical
apply regressed from 36.923 s to 38.065 s. SQLite still probes the unique timeline index
inside an insert, so the explicit 13.275 s lookup timer was not an independently removable
cost. The five campaign cardinalities matched exactly; the performance gate failed before
ranked-search comparison, and `c6319e6` reverted the candidate. The current path therefore
retains measured ceilings of 13.275 s for message identity lookups, 9.327 s for message
writes, 4.286 s for tool-result writes, and 1.034 s for session touches. These are costs,
not additive saving
promises. The corpus now has one non-empty SQLite snapshot source, but it is not large
enough to transfer JSONL batching or throughput results to Hermes/OpenCode; those claims
still require a driver-specific frozen pair or a declared production-weighted agent mix.

- sonic-rs (or simd-json) for JSONL; rayon parallelism across files for backfill;
  blake3 for content hashes and ledger-frame checksums.
- Batched writes; prepared-statement caches; mmap reads for big transcripts.
- Private cold loads and a normal fresh `serve` capture adapters into independent RAM planners,
  precompute presentation order, and feed closed runtimes to one canonical writer. Fresh serve
  places that canonical schema in named shared memory: metadata is immediately public and each
  completed runtime becomes transactionally readable before the global set-based gate. It then
  installs one validated disk copy and atomically retargets readers. Cross-session aggregates are
  unavailable until that retarget, and the app bounds their post-publication fan-out. Existing
  complete projections enter live maintenance immediately and run only bounded metadata plus forced
  cursor-relative catch-up; legacy incomplete disk projections keep the per-file WAL resume path.
  FTS is always a detached generation. Live batches
  and replacement swaps remain transactionally canonical and rebuild each distinct affected usage
  bucket once.
- Detached search construction resolves message/tool column positions once per SELECT and reuses
  the ordinary typed decoder and its validation. Single-row callers retain named-column decoding;
  cached positions are derived from the actual statement, including session-scoped repair scans.
  The unpublished FTS generation uses a 16 MiB pager cache, released when its build connection
  closes. Search extraction, member verification, FTS integrity checks, SQLite quick checks, syncs,
  and atomic generation publication remain part of the measured work. SQLite documents this
  [connection-local, demand-allocated cache setting](https://www.sqlite.org/pragma.html#pragma_cache_size).
  Frozen campaign `rosetta-cold-ingest-20260906` compared the retained `7e6b873` working-tree
  binary with this change on an Apple M1 Max / 64 GiB, macOS 26.5.2 arm64, Rust 1.97.1 release,
  system volume, warm filesystem cache. It reused 200 read-only Claude Code/Codex JSONL files,
  200,000 records / 316,975,655 bytes; source manifest SHA-256
  `f7c85b319eeb1779a0e29f3dba39bc6be29a221cda09a082109c37b3a3b2fb93`.
  Three alternating pairs used `ingest --experiment-parallel-cold
  --experiment-cold-completion-order --experiment-async-ledger --experiment-sqlite-in-memory`
  with identical enrollment, source snapshot, and profiling. Full-command median elapsed fell
  **13.718 → 11.677 s (−14.9%)**, with ranges 13.510–15.258 and 11.021–12.548 s;
  detached-search median **8.175 → 5.710 s (−30.2%)**; median peak RSS
  **1,448.9 → 1,421.4 MiB**. An additional ordinary fresh-`serve` pair, polled through
  `/api/status` every 50 ms without a renderer, reached search-ready at 12.852 → 10.577 s;
  canonical-ready was 5.801 → 5.292 s. This is corroborating evidence from one readiness pair.
  All six finite builds, both fresh
  serves, and a candidate replay of the frozen baseline ledger have identical normalized digests
  across 17 canonical tables and all five search lanes: 120,151 messages/search documents,
  200 sessions, 5,485 file edits, 836 usage buckets, zero drift. Normalization excludes only
  process-time/audit/checkpoint bookkeeping and maps surrogate IDs to stable identities.
  The required `search-bench` comparison passes with 16 product-grammar diagnostic cases
  (six return hits), five warmups and 100 measured cycles in each shared/fresh backend lane:
  6,400 measured queries across the pair, identical counts and ranked-result digests. Its frozen
  campaign is `bedd27c9d6e301126edcdcce72b1d09ece99328318bcf980ef85ce2955fa3f27`;
  query-suite SHA-256 is `9ce28aa99fe0eeda42a3769872b46deb993200b1e696424e3b40f4ee076ace44`.
  This diagnostic mix does not establish production query weighting. Workspace validation passes
  842 tests (one opt-in watcher test ignored); formatting passes, and workspace/all-target Clippy
  passes with only the existing `large_enum_variant` category excluded.
  Initial exploratory runs that exhausted disk headroom were discarded and both sides rerun;
  retained repeat databases are losslessly archived with verified per-file hashes.
- A first cold projection now requires both the projection and ledger destination to be
  absent. It buffer-flushes into a same-filesystem named `<ledger>.staged` tree whose durable
  manifest preserves enrollment/root binding across process exits, syncs every staged segment
  plus child-to-parent directory entries, atomically promotes the whole tree,
  syncs its parent, and only then installs the validated SQLite file. A missing projection
  beside an unmarked existing ledger normally fails closed to `rebuild`, preventing generation
  collisions. A marked staged/public cold generation instead replays sealed evidence into RAM and
  finishes raw-only sources at a strictly newer generation. `--restart-incomplete` archives only
  explicit legacy artifact sets under the projection lock.
  Existing/live projections retain ledger-sync-before-visible-SQLite ordering. The frozen
  pair measured segment syncs at 7,435 → 66 and ledger time at 96.64 s → 18.02 s; twelve
  directory barriers cost 0.0005 s. The zstd container's pair (2026-09-04; synthetic
  claude-code corpus of 400 files / 113,006 records / 1.114 GB with a sha256 manifest, seed
  20260904; release builds of de8f074 vs ac4f142 in separate worktrees; Apple M1 Max carrying
  unrelated load 67→15; three alternating rounds, page cache warmed): ledger on disk
  1,118,207,122 → 199,120,503 bytes (5.62×); ingest wall 42.2/39.6/43.7 s → 47.9/40.1/42.8 s
  (rounds 2–3 within ±2%), ledger worker time 2.4–4.0 s → 4.5–5.3 s, user CPU +3.5 s,
  peak RSS 2,410 → 2,408 MiB; rebuild 34.9/35.8/37.8 s → 54.5/37.4/38.9 s with ledger read
  2.4 s → 3.7–5.1 s per replay (the decode; the raw side read from cache); 113,807 frames,
  146,636 ops, and every projection table equal on all six runs. So: storage 5.6× smaller,
  ingest unchanged within noise because the work lands on the ledger worker, and a
  warm-cache rebuild a few percent slower — the cold-cache case, where 5.6× fewer bytes
  come off disk, was not measured. Search is independently disposable and may never
  delay canonical correctness validation.
- Every `src_cursor` reconciliation predicate has a matching index. Query-plan tests pin
  these indexes because one missing provenance index turns source activation quadratic.
- A performance optimization is merge-admissible only with a frozen before/after pair
  from the same permission-read-only checksummed source snapshot for cold-write work or
  the same ledger for replay work, plus one checksummed query suite, campaign ID, machine,
  configuration, logical projection counts, and ranked-result digests. Estimates and
  unpaired organic runs remain planning evidence. The first search step is the FTS5
  p50/p95/p99 baseline; a Tantivy bake-off starts only if the declared or observed query
  mix misses the product objective or relevance/availability requires it.
- Memory-first fan-out for live streams (§5.2); DB commit is not on the notify path.
- Budgets asserted in CI-able bench: full `~/.claude` cold backfill < 5 s per GB of
  transcripts; watch-event → subscriber frame p99 < 20 ms; search p99 < 50 ms warm.

## 8. Workspace layout

```
rosetta/
├── Cargo.toml                # Rust workspace; every member is under crates/
├── CLAUDE.md                 # project rules for Claude sessions
├── rust-toolchain.toml       # pinned toolchain
├── docs/
│   ├── ARCHITECTURE.md       # this document (living)
│   └── groundtruth/
│       ├── RESEARCH_PLAN.md  # per-agent sourcing plans + survey (living)
│       └── PATTERNS.md       # Phase-2 cross-agent synthesis (future)
├── crates/
│   ├── adapter-support/      # permissive raw-parser + replay-test primitives
│   ├── core/                 # canonical model (hypothesis), ids, versions   (L2 heart)
│   ├── capture/              # watcher, tailers (tail-append JSONL / whole-file
│   │                         #   JSON / SQLite WAL), cursors, raw ledger     (L2 runtime)
│   ├── adapters/             # one production parser crate per captured agent
│   │   ├── claude-code/
│   │   └── codex/
│   ├── pipeline/             # ingest orchestration, analyzers, replay       (L2 runtime, Phase 3)
│   ├── store/                # canonical SQLite projection + search repair log (L2 persistence, Phase 3)
│   ├── search/               # detached FTS5 sidecar + publication worker   (L3 search boundary)
│   ├── protocol/             # wire types (serde + ts-rs), versions          (L3 contract, Phase 4)
│   ├── server/               # axum REST + WS services                       (L3 runtime, Phase 4)
│   ├── search-bench/         # frozen-input search percentile campaigns     (evidence)
│   └── rosettad/             # binary: daemon + CLI (run, backfill, probe,
│                             #   replay-diff, search, doctor)
├── groundtruth/              # DATA ONLY; one directory per roster agent
│   ├── claude-code/          # layout, sources, fixtures, canonical JSON+HTML
│   ├── codex/
│   └── …                     # watch.toml first; evidence added breadth-first
├── tools/groundtruth/        # stdlib Python evidence pipeline               (Phase 1)
├── clients/                  # generated consumer packages, e.g. typescript/ (Phase 4)
└── .github/workflows/        # ci.yml; release-watch.yml when automation is enabled
```

Dependency rule (enforced, not aspirational): `adapters → {core, adapter-support}`;
`pipeline → {core, capture, store, adapters}`; `server → {core, protocol, store}`;
**`core` and `protocol` depend on almost nothing**; nothing depends on `rosettad`.
Python tools read and write ground-truth data but are outside the Cargo graph.
Production crates never import development tooling or intermediate evidence.

## 9. Testing strategy

1. **Fixture corpus** per agent per version, replayed in CI (parse → normalize → golden).
2. **Differential replay**: new normalizer vs old over the full ledger, diff report.
3. **Tailer property tests**: random truncation/rewrite/partial-line torture.
4. **Contract goldens**: recorded observe/metrics WS sessions per protocol version.
5. **Probe in anger**: run `rosetta probe` against the live home dirs after every agent
   update; drift report drives fixture refresh.

## 10. Roadmap — layer by layer, breadth-first

Vertical feature slices built on one agent are how spaghetti overfit its canonical model
to Claude Code — every later adapter had to contort to fit. Rosetta is built **layer by
layer**, and each layer is validated across the whole agent roster before the next
layer's design is frozen.

- **Phase 1 — Ground truth (L1), across all mainstream agents.**
  Execute [groundtruth/RESEARCH_PLAN.md](groundtruth/RESEARCH_PLAN.md) over the roster
  (Claude Code, Codex, Grok, Antigravity, Copilot CLI, OpenCode, Kimi CLI, Hermes, Pi,
  …): per-agent layout manifests, versioned raw schemas, fixture farm, and release-watch
  automation.
  *Exit: every roster agent has a committed layout manifest, raw types, per-version
  fixtures, and green parse-replay in CI.*
- **Phase 2 — Pattern synthesis (L2 design).**
  A cross-agent comparison distills the common shapes (sessions, entries, tool calls,
  usage metrics, artifacts) and their per-agent variance; the §4 canonical model is
  revised from that evidence and ratified as v1, with per-agent mapping notes.
  *Exit: canonical model validated against the full roster's ground truth.*
- **Phase 3 — Runtime ingestion + normalizer (L2), across all agents.**
  Watcher/tailers, raw ledger, `normalize()` per adapter, analyzers, store
  projection, and differential replay.
  *Exit: the entire fixture corpus normalizes clean; goldens stable.*
- **Phase 4 — Service provider (L3).**
  Search, observe, metrics over the now-stable canonical store; generated TS clients;
  consumer migrations (agent-view app, chopsticks).
  *Exit: spaghetti decommissionable.*

## 11. Open decisions (defaults chosen, cheap to revisit)

| Decision | Default | Revisit when |
|---|---|---|
| FTS5 vs Tantivy | detached FTS5 sidecar today; Tantivy prototype remains benchmark-only | revisit with judged relevance + declared/observed query mix + accepted storage margin; FTS5 coherence/crash gates are already implemented |
| WS vs SSE for streams | WS (bidirectional subscribe/ack) | if consumers end up read-only-simple |
| Ledger retention | keep forever; segments zstd-compressed since 2026-09-04 (`ledger compact` for older ones); no segment GC yet | compressed ledger > tens of GB |
| Image-member retention | keep immutable content-addressed members (no GC yet) | `.assets/` growth justifies a ledger-derived mark/sweep under the projection lock |
| Grok/Codex priority | after M2 | chopsticks needs them sooner |
| UDS transport | not in v1 | localhost TCP shows up in profiles |
