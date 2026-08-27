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
   projection. The DB is disposable — rebuilt from the ledger, never migrated.
5. **Parsers exist once, in Rust.** Consumers get generated types (ts-rs from
   `protocol`). Never hand-write a second parser in another language.
6. **One ground-truth file per agent.** An agent's ground truth is solely and
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
recovery, and an append-only checksummed raw ledger. `rebuild` replays the complete
ledger into a private projection and atomically installs it only after validation;
fresh ingest/watch backfills use the same private cold-load path, deferring FTS,
rollups, and read indexes until publication. Live batches retain same-transaction
derived-state maintenance with indexed provenance and coalesced bucket repairs;
ordinary schema-version mismatches still fail closed so rebuild is always explicit.
The store hot tier, in-process subscription hub, localhost HTTP/WebSocket transport,
backend-neutral protocol, and generated TypeScript bindings are implemented. Consumer
migrations and the two uncaptured roster agents remain Phase 4/Phase 1 work.

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
  rosettad/                discover, ingest, watch, serve, and atomic rebuild CLI
  search-bench/            frozen-input FTS percentile campaign harness
clients/                   generated TypeScript protocol package      [Phase 4]
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
