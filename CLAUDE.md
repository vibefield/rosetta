# Rosetta

Multi-agent data ingestion engine in Rust — the ground-up rewrite of `p008/spaghetti`.
Three jobs: full-text search over agent transcripts; live session/metadata observation
for the agent-view app; live distilled metric feeds for runtime monitors (chopsticks).

Read `docs/ARCHITECTURE.md` before designing anything. Read
`docs/groundtruth/RESEARCH_PLAN.md` before touching any adapter — it holds the
surveyed per-agent sourcing plan (11 agents, tiered, with citations).

## Iron rules (each one is a spaghetti failure mode — do not relitigate)

1. **Layer separation.** L1 ground truth (data under `groundtruth/`; dev-time
   Python evidence tools under `tools/groundtruth/`; PRODUCT = one canonical
   `groundtruth.json` plus generated `groundtruth.html` per captured agent) →
   L2 runtime (`capture`, per-agent parsers in `crates/adapters/*` designed against the
   common interface with the L1 file as their SPEC — never against fixtures or
   intuition — plus `core`, `pipeline`, `store`) → L3 services (`protocol`,
   `server`). Data flows up only. ALL knowledge about a specific agent lives in
   its pair — `groundtruth/<agent>` (layout and evidence) and
   `crates/adapters/<agent>` (parser); its paths, formats, quirks never leak
   into other crates. Consumers never reach past L3 into the store.
2. **Layer-first, breadth-first.** Never design or freeze an L2 abstraction from one
   agent's shape. The canonical model in `core` is a HYPOTHESIS until Phase 2
   ratifies it against all 11 agents' ground truth. Phases = layers, never vertical
   feature slices on a single agent.
3. **Machine-independent ground truth.** Schemas come from agent source code, release
   channels, and hermetic fixtures. CI enforcement is fixture-only. Closed-source
   agents (no source channel) additionally use `Evidence::Observed`: committed
   structure-only censuses of large organic datasets (field names/types/counts,
   never content — see `groundtruth/<agent>/census/`) plus spaghetti's battle-tested
   parsers as prior art. Raw local data itself is never committed and never
   load-bearing in CI.
4. **Two data representations only:** append-only raw ledger + one canonical
   projection (SQLite plus content-addressed binary members). The projection is
   disposable — rebuilt from the ledger, never migrated.
5. **Persist only what a user cares about.** Normalization is deliberately lossy:
   it cherry-picks the locked, product-defined canonical fields and leaves every
   other native value in the raw ledger. Agent-specific fields, unfamiliar tools,
   progressive snapshots, candidate sets/orderings, correlation candidates, opener copies,
   per-field merge histories, and other reconciliation machinery may exist transiently
   in RAM, but must never become canonical SQLite columns or tables. A new agent shape
   may update that agent's parser; it may not expand the canonical model without an
   explicit product decision. Rewrites and restarts rebuild the affected projection
   scope from the ledger instead of persisting a second fact journal in SQLite.
   Minimal bounded cursor/provenance (including provenance for one selected user-facing value),
   readiness, and coalesced search-repair watermarks
   may persist for their named correctness consumers, but they may contain identifiers
   and counters only—never native payloads, normalized op JSON, candidate values, or
   merge history.
   **Session ownership is metadata-only:** every adapter must settle one normalized primary
   `cwd` and at most one local checkout project from its bounded session metadata source.
   Adapters may identify that project by a checkout-group root or by another native session;
   the shared meta resolver, never an adapter, mints the canonical id. Ordinary clones stay
   separate while linked Git worktrees converge. If the source cannot prove ownership, it
   stays absent. Full-stream records may carry a changing cwd
   transiently to resolve relative tool arguments, but may never create a project, reassign a
   session, persist workspace observations, or revise the canonical cwd.
6. **Parsers exist once, in Rust.** Consumers get generated types (ts-rs from
   `protocol`). Never hand-write a second parser in another language.
7. **One ground-truth file per agent.** An agent's ground truth is solely and
   fully reflected in `groundtruth/<agent>/groundtruth.json` — the ONLY
   source of truth, rebuilt by `build_canonical` from the intermediates in the sibling
   `sources/` dir (fixture captures, censuses, SQL dumps). Intermediates
   contribute; they are never authoritative. Never read schema truth from an
   intermediate, and never edit the canonical file by hand.

## Current phase

**Phase 3 is underway, with Phase 1 gaps still explicit.** Nine agent data packs and
runtime adapters are registered in `rosettad`. The generic pipeline handles the
JSONL and SQLite snapshot drivers those adapters select, generation-safe rewrites,
deterministic adapter checkpoints, watch-before-scan live ingestion, polling/audit
recovery, and an append-only checksummed raw ledger whose segments are zstd streams
(`RLZ1`, since 2026-09-04; plain `RLG1` is read forever and `rosettad ledger compact`
rewrites it in place) with a derived per-runtime `.segments` index for scoped reads.
Fresh/resumed RAM cold serve writes plain segments first. After canonical/search readiness,
one background worker compresses only closed segments, targeting 10% of one core with
macOS utility QoS. Atomic verified replacement preserves concurrent reads and crash
recovery; interrupted maintenance resumes from remaining plain segments on a warm start.
A normal fresh `serve` binds HTTP/WS
before discovery, then constructs the canonical schema in a named shared-memory SQLite
database. Metadata is planned off the writer thread, a few runtimes at a time; each enrolled
adapter's bounded, source-free metadata batches commit there in registry order and drain
through `Hub::absorb_meta`, and that runtime's producer starts capturing at once, so projects
and top-level sessions are immediately public while payload ingestion continues and no
runtime's capture waits for a later runtime's skim. Adapters stream, ledger, and normalize in
parallel through private RAM planners, handing each source to the sole canonical writer as a
closed source (terminal generation, final checkpoint) in path order as soon as nothing can
supersede it — a session is held only while a restart's sibling heal could still replace its
transcripts — under a 256 MiB in-flight budget. One capture pool shared by every runtime (a
worker per core but one by default; `--capture-workers 0` restores one producer reading its
runtime's files in turn) reads, normalizes and frames the fresh JSONL sources a batch at a
time ahead of each producer, which becomes its runtime's sequencer and commits them strictly
in path order, so ledger bytes, planner and closed sources equal the serial walk's for any
worker count; look-ahead is byte-bounded and parks rather than blocks. On its way out, each
producer's `StreamedPreparer` ranks final ordinals and withholds each session's
tool/usage/file-edit facts until the session's last roster source; a session no other source
crossed is folded right there (the admission fold's own code) and its resolved tool rows
travel with that source, while a crossed session's facts go back unresolved. The writer lands
those final rows — and the resolved tool rows, each written once — in the canonical RAM
generation while capture continues; whatever is left collects into that runtime's own RAM
fold, never another's. It admits only a complete, clean runtime: at its producer's close the
fold writes each remaining tool row once, and only its final readiness commit (the Pending
flip) makes the canonical timeline visible through the ordinary REST/WS queries — every
payload read route honours Pending. No runtime is
retained whole in RAM. Open session subscriptions receive one resync barrier; parser
fragments and reconciliation state are never public. This is deliberately runtime-granular:
cross-file relations cannot expose a half-reconciled session merely to manufacture finer UI
progress. Finite `ingest` keeps the retained closed-runtime path.

After every runtime closes, the daemon performs set-based finalization and full validation in
RAM, drains and fsyncs the staged raw ledger, backs the canonical database up to a
same-filesystem private file, syncs and atomically installs it, then switches the read router
from RAM to disk before releasing the RAM generation. Detached search remains explicitly
Building and does not start until that switch; it wakes right after it, before any runner's
catch-up, whose commits become ordinary revision-stamped repairs. Watchers are armed before metadata, and their
queued suffixes flow into the ordinary live runner after publication. Cross-session aggregates
(heatmap, usage decomposition, and contribution) return protocol-v7 `not_ready` while the reader
targets shared RAM; this prevents presentation scans from contending with the sole canonical
writer. The renderer waits for `canonical_ready` and hydrates project aggregates with bounded
concurrency after publication. An existing complete projection is live immediately: watchers arm
before one bounded current-inventory skim, then `LiveRunner::catch_up` drains queued hints and forces
one cursor-relative stat poll before the ordinary cadence. It does not re-enter cold/backfill state.
Polls compare files with each source's committed cursor evidence (normalizer, observed length) held
in RAM: seeded from the cold writer's final checkpoints on a fresh start, otherwise read from a cursor
row once, and forgotten whenever the runner streams that source.
`--stop-after-meta` and legacy incomplete disk projections retain the disk-progressive `ColdEngine` path: its four lanes
(live ▸ boost ▸ starter ▸ completion), slice budget, boost queue, and scoped row effects remain
the recovery/audit topology. Finite `ingest` and explicit `rebuild` remain private, validated,
atomic publication paths and never slice. An idle daemon
appends nothing to the raw ledger: the rotating audit verifies a file's committed
content chain (tail checkpoint v2 — a resumable byte fold; pre-chain checkpoints are
established in place) and replays only on a real mismatch; snapshot passes stat-gate an
unchanged database without opening it (a read-only SQLite open writes `-shm` inside the
watched root, and the daemon must never hint itself — `-shm` is never a hint); poll
admissions are evidence-driven (first sighting or movement, never presence). Audit-budget
passes still hash snapshot content whole, so a stamp-preserving rewrite is caught by the
audit sweep. The resumable disk cold phase is one named unit
(`rosettad::engine::ColdEngine`) built over an `IngestPlan` — meta's
settled projects → sessions → sources as a RAM value, unproven ownership honestly
unassigned — and a `ProgressBoard` that folds pass outcomes per project onto
`/api/status` (`ingest.projects`, protocol v7); plan, board, and schedule are RAM only
and rebuilt from a skim, while durable progress stays cursors, generations, and
`stream_state`.
Starter and completion passes carry a byte-governed slice budget (`--slice-budget`,
default 1 MiB / 24-record floor — the measured knee of the 2026-08-31 slice-sweep
campaign — batches soft-capped to the slice) that early-ACTIVATES
and seals a clean prefix so every session opens readable fast; paused remainders requeue
as append growth, app boosts run unbudgeted with preemption bounded by one slice, and a
session badges Indexed only once no task for it remains queued. Snapshot-driver sources
always stream whole (revision = whole-content hash); a source that turns unclean mid-pass
falls back to whole-file handling. Dirty sources converge on live-growth semantics on
every cold path: a dirty FIRST generation activates its parseable rows with the session
held Streaming (never Indexed) — identical sliced, unsliced, or rebuilt — while dirty
replacements still abandon. `milestone:` stdout lines feed the sweep harness.
Interrupted disk-progressive work is durable and resumable. Interrupted fresh RAM work never
installs an incomplete canonical database; it retains a named `<ledger>.staged` generation with a
fsynced manifest binding projection, ledger, enrollment, and runtime roots. The next plain `serve`
replays sealed evidence into fresh canonical RAM, ignores raw-only tails, reserves their generation
in RAM so source retries allocate strictly above it, finishes from the current source inventory, and
publishes the same generation. The recovery marker survives ledger promotion and is removed only
after SQLite installation. The app does not pass `--restart-incomplete`; that flag remains an
explicit legacy archive/restart operation for old incomplete projection sets. A complete projection
is never moved. `pnpm dev` builds `rosettad --release` and
passes that exact binary to Electron; mtime-based debug/release selection is forbidden.

Projection schema 13 keeps user-facing canonical rows, rollups, and joins in SQLite while
production FTS5 lives in a disposable detached sidecar. The asynchronous
worker, `projection_state`, and coalesced `search_dirty` consumer landed together; epoch,
revision, acknowledgement, crash, and repair tests prevent stale or cross-projection hits.
Verified inline image bytes are decoded once into immutable `<projection>.assets/` members;
SQLite retains only relocatable `/api/assets/<digest>.<ext>` references and the raw ledger
remains lossless.
Each session now carries exactly one adapter-selected primary cwd and one optional project link
from bounded meta ingest. `workspace_obs` and timestamp-based stream reassignment no longer exist;
local checkout groups are resolved once in the ledgered meta batch; ordinary clones of one remote
stay distinct, linked worktrees converge, and nested working directories do not manufacture
projects. A source-scoped stream batch containing `SessionMeta` is invalid and
must fail closed at the store boundary.
The closed user-facing timeline and fixed semantic tool vocabulary are implemented across all
captured adapters. Native payload noise and unsupported tool fields remain ledger-only. Tool
fragments, evidence ranks, late usage, and complete replacement generations reconcile in RAM;
SQLite has no normalized-op staging, pending-merge, opener-copy, per-field stamp, or tool-fact
journal. Cold ingest writes one resolved tool row, live ingest updates only that resolved row, and
a lost accumulator is recovered by affected-scope ledger replay.
Canonical and search readiness are independently surfaced. The store hot tier,
subscription hub, localhost HTTP/WebSocket transport, backend-neutral protocol, and
generated TypeScript bindings are implemented. A feature-gated Tantivy prototype remains
benchmark-only pending judged relevance, product query weighting, storage acceptance, and
production lifecycle evidence. Consumer migrations and the two uncaptured roster agents
remain Phase 4/Phase 1 work.

## Repo map

```
groundtruth/               LAYER 1 data only                           [Phase 1]
  <agent>/                 watch.toml, layout.toml, fixtures/, census/,
                           sources/, canonical groundtruth.json, and
                           generated groundtruth.html
tools/groundtruth/         LAYER 1 dev tools (stdlib Python): fixture farm,
                           mock model, census, schema merge, HTML render,
                           boundary/artifact checks
tools/ingest-bench/        local Electron cold-ingest profiler + read-only DB inspector
crates/                    all Rust crates
  adapter-support/         permissive parser + replay-test primitives
  adapters/<agent>/        raw parsers using canonical ground truth as spec
  core/ capture/           LAYER 2 model, discovery, JSONL/SQLite capture, ledger, watch
  pipeline/ store/         LAYER 2 backfill/live/rebuild orchestration + projection/hot tier
  protocol/ server/        LAYER 3 wire contract + axum REST/WS runtime
  search/                  backend-neutral query/readiness seam + detached FTS5 worker
  rosettad/                discover, ingest, watch, serve, and atomic rebuild CLI
  search-bench/            frozen-input FTS percentile campaign harness
clients/                   TypeScript consumers                       [Phase 4]
  ts/                      generated protocol types + RosettaClient SDK
  app/                     agent-history app — Electron + React History room,
                           design source: protos/agent-history-proto; the locked
                           transcript record ships in clients/app/design/
```

## Conventions

- Raw serde types are **permissive**: every struct carries
  `#[serde(flatten)] unknown: BTreeMap<String, Value>`; non-empty `unknown` emits a
  drift fact. `deny_unknown_fields` is banned — agent releases must never break
  parsing.
- `normalize()` is pure: no IO, no clock, no randomness. IDs are deterministic
  (blake3 of stable inputs).
- Fixtures are the proof: never assert a schema fact without a fixture exercising
  it; never point tests at live home directories.
- Keep modules under ~1,000 lines — split before they grow (spaghetti had a
  14,102-line `projection.rs`).
- A new DB table and its named code consumer land in the same commit. In particular,
  `projection_state` / `search_dirty` do not land before the asynchronous search worker
  that consumes and acknowledges them.
- Every performance optimization commit cites a frozen before/after pair from the same
  permission-read-only checksummed source snapshot for cold-write work or the same ledger for
  replay work, plus one checksummed query suite, campaign ID, machine, and configuration, with
  logical counts and ranked-result digests equal. Estimates and
  unpaired organic runs do not authorize merge; if an artifact is deleted before the
  pair exists, rerun that side.
- Docs discipline: TWO living documents (`ARCHITECTURE.md`, `RESEARCH_PLAN.md`),
  updated in place. No RFC sprawl (spaghetti had 60+ RFCs contradicting each other).
- Edition 2024, workspace deps, `cargo fmt` + `clippy -D warnings` clean.
- `groundtruth/` is data-only. Rust belongs under `crates/`; repository tooling
  belongs under `tools/`.

## Commands

- `cargo check` / `cargo test --workspace` / `cargo fmt --all` /
  `cargo clippy --all-targets -- -D warnings`
- Ground-truth pipeline: `python3 -m tools.groundtruth farm <agent>` (isolated runner
  required unless `--schemas-only`), `census`, `render`, and `check`.
- Python tool tests: `python3 -m unittest discover -s tools/groundtruth/tests`.
- Frozen FTS campaigns: `cargo run -p rosetta-search-bench -- help` (protocol and
  privacy rules in `crates/search-bench/README.md`).
- Ledger compaction (stop the daemon/app that owns the projection first):
  `rosettad ledger compact --db PATH [--ledger DIR] [--level N] [--long] [--force] [--dry-run] [--json]`.
- Ledger evidence (read-only; per-segment decoded digests plus an entry digest that ignores
  containers, segment boundaries, file ids and batch packing, split into source and meta
  digests because metadata carries discovery times):
  `rosettad ledger verify --db PATH | --ledger DIR [--json]`.
