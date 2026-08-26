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
parser crates are registered. The implemented backfill pipeline/store currently admit
Claude Code and Codex JSONL transcript streams; the remaining adapters are parser-only.
The normalized source-op journal provides generation-safe replay inside the current
projection, but it is not the append-only raw ledger promised by rule 4. Until that
ledger and rebuild command ship, schema-version mismatch must fail closed. Watch/live
ingestion and the `protocol`/`server` service surface remain unimplemented.

## Repo map

```
groundtruth/               LAYER 1 data only                           [Phase 1]
  <agent>/                 watch.toml, layout.toml, fixtures/, census/,
                           sources/, canonical groundtruth.json, and
                           generated groundtruth.html
tools/groundtruth/         LAYER 1 dev tools (stdlib Python): fixture farm,
                           mock model, census, schema merge, HTML render,
                           boundary/artifact checks
crates/                    all Rust crates
  adapter-support/         permissive parser + replay-test primitives
  adapters/<agent>/        raw parsers using canonical ground truth as spec
  core/ capture/           LAYER 2 model, discovery, bounded reads, JSONL tailing
  pipeline/ store/         LAYER 2 backfill orchestration and SQLite projection
  protocol/ server/        LAYER 3 placeholders (service not shipped)
  rosettad/                discover + ingest CLI (daemon not shipped)
clients/                   generated consumer packages (TS)          [Phase 4]
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
- A new DB table requires a named consumer. Target ~15 tables total, not 101.
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
