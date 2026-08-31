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
   `cwd` and at most one project/repository from its bounded session metadata source. If the
   source cannot prove them, they stay absent. Full-stream records may carry a changing cwd
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
recovery, and an append-only checksummed raw ledger. Fresh `serve` creates the public
projection immediately: it skims every enrolled runtime first, then cold-streams through
a three-lane scheduler (boost ▸ starter ▸ completion, runtime round-robin within a lane).
Starter and completion passes carry a byte-governed slice budget (`--slice-budget`,
default 256 KiB / 24-record floor, batches soft-capped to the slice) that early-ACTIVATES
and seals a clean prefix so every session opens readable fast; paused remainders requeue
as append growth, app boosts run unbudgeted with preemption bounded by one slice, and a
session badges Indexed only once no task for it remains queued. Snapshot-driver sources
always stream whole (revision = whole-content hash); a source that turns unclean mid-pass
falls back to whole-file handling, and the sliced-vs-unsliced dirty-file content rule is
an explicitly pinned open decision. `milestone:` stdout lines feed the sweep harness.
Interrupted progress is durable and resumable. Finite `ingest` and explicit `rebuild`
remain private, validated, atomic publication paths and never slice.

Projection schema 12 keeps user-facing canonical rows, rollups, and joins in SQLite while
production FTS5 lives in a disposable detached sidecar. The asynchronous
worker, `projection_state`, and coalesced `search_dirty` consumer landed together; epoch,
revision, acknowledgement, crash, and repair tests prevent stale or cross-projection hits.
Verified inline image bytes are decoded once into immutable `<projection>.images/` members;
SQLite retains only relocatable `/api/assets/<digest>.<ext>` references and the raw ledger
remains lossless.
Each session now carries exactly one adapter-selected primary cwd and one optional project link
from bounded meta ingest. `workspace_obs` and timestamp-based stream reassignment no longer exist;
repository roots are resolved once in the ledgered meta batch and nested working directories do
not manufacture projects. A source-scoped stream batch containing `SessionMeta` is invalid and
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
                           design source: protos/agent-history-proto
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
