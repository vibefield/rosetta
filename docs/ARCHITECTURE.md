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

> **Implementation checkpoint (2026-08-26).** This document describes the target
> architecture. Nine ground-truth/runtime-adapter packs are registered. The generic
> runtime handles adapter-selected JSONL and SQLite sources, generation-staged
> reconciliation, deterministic adapter checkpoints, FTS and rollups, live watching
> with polling/audit recovery, a checksummed append-only raw ledger, and validated
> whole-projection rebuild with atomic installation. Fresh backfill and explicit rebuild
> use a private projection: FTS, usage buckets, and read-only indexes are built once,
> validated, and atomically published. Live batches coalesce repairs by
> `(session, hour, model)`, and generation sweeps use indexed provenance. `source_op` is temporary
> projection-internal rewrite staging, not a third durable representation. The store
> hot tier and in-process subscription hub are implemented; HTTP/WS transport and
> generated clients are pending. Unknown projection schemas fail closed on ordinary
> open and are replaced only through the explicit ledger rebuild command.

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
│      REST /v1/search, /v1/sessions …                   server       │
│      WS   /v1/observe   (snapshot → deltas, resumable)              │
│      WS   /v1/metrics   (live distilled feed)                       │
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
  ~20 ms coalescing, periodic audit sweep to catch anything missed.
- **Tailer**: per-file cursor = byte offset + line count + hash of a head prefix.
  The head hash detects rewrite/truncation (Claude Code rewrites files on compaction);
  on mismatch, the file re-ingests under a new **epoch**. Partial trailing lines are
  buffered, never half-parsed.
- **Raw ledger**: every complete raw record is appended with source/generation metadata
  to size-rotated segment files, checksummed with blake3, and durably flushed before the
  corresponding projection transaction. By default `rosettad` stores one runtime
  ledger at `<projection>.ledger/<agent>/`; `--ledger` overrides that root. Segments are
  self-describing and scanned incrementally during rebuild. Two reasons this exists:
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
> The current detailed hypothesis — ten entities, the flattened message-row taxonomy,
> the discover/meta/stream ingest phasing, and the concrete SQLite DDL, ratified
> against 9 of 11 ground-truth packs — lives in
> [`docs/canonical-model.html`](canonical-model.html) (rendered design doc, 2026-08-26).
> Where that file and this section disagree, the HTML is newer.

The canonical model is deliberately small — it covers what the three jobs need, nothing
more:

```rust
enum CanonicalFact {
    SessionUpserted(Session),     // agent kind, project/workspace, native id, times, status
    EntryAppended(Entry),         // role, kind (message/thinking/tool_use/tool_result/system),
                                  //   text content, tool payload, parent linkage, timestamps
    ArtifactUpserted(Artifact),   // todos, plans, tasks, team state
    MetricEmitted(MetricSample),  // e.g. Usage { input, output, cache_creation, cache_read }
                                  //   keyed (session, response_key), canonical flag
    LifecycleEvent(Lifecycle),    // session start/end, compaction, subagent spawn/finish
    DriftObserved(Drift),
}
```

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
`GET /v1/search?q=&agent=&project=&session=&from=&to=&kind=` → FTS5 match with snippets,
highlights, rank; filterable by agent/project/time/entry-kind. Paged, cursor-based.

**Observe (job 2)** — the flagship contract, successor to `observeSession`:

```
WS /v1/observe?session=<id>&from=<cursor>
  server → { hello, protocol: "rosetta-observe/1", epoch, seq }
  server → { snapshot }                       # full session state at seq
  server → { delta, seq: n } …                # monotonic per-epoch
  server → { resync }                         # client gap or epoch change → resnapshot
```

Snapshot-then-delta with resumable `(epoch, seq)` cursors, same concepts chopsticks
already implements (epoch / overflow / resync). The contract is versioned **in the
handshake**, its types live in `rosetta-protocol`, and a generated TypeScript package
(`ts-rs`) is published semver'd — never again a `link:` dep pinned by string, never
again a silently breaking revision-id derivation. Golden contract tests freeze the wire
format per protocol version.

`GET /v1/agents`, `/v1/sessions`, `/v1/sessions/{id}` serve the metadata/browse side of
the agent-view app.

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
  batched transactions. One transaction per ingest batch: facts + FTS + rollups +
  `change_log` row — consumers can also long-poll/replay the change log as a coarse
  fallback stream.
- **FTS5 external-content** over entry text (porter + trigram experiments to be
  benchmarked). Start with FTS5; **tantivy only if** FTS5 relevance or latency measurably
  fails us — one store keeping search transactionally consistent with metadata is worth
  a lot.
- **On-disk layout:** `<projection>.ledger/<agent>/` (segments) + the configured SQLite
  projection; `--ledger` can relocate the segment root. Ledger is precious; DB is
  disposable.
- Target table count: **~15, not 101.** Entities, rollups, ledger index, change log,
  drift, cursors. A new table needs a consumer to justify it.

## 7. Performance playbook

> The detailed implementation design — the pure op-stream pipeline, crate seams,
> stream-phase concurrency, and the bench/profiler infrastructure (bench-corpus
> generator, per-stage criterion benches, `rosettad ingest --profile`, the rebuild
> headline bench) — lives in [`docs/ingest-engine.html`](ingest-engine.html)
> (rendered design doc, 2026-08-26). Where it and this section disagree, the HTML
> is newer.

- sonic-rs (or simd-json) for JSONL; rayon parallelism across files for backfill;
  blake3 for content hashes and ledger-frame checksums.
- Batched writes; prepared-statement caches; mmap reads for big transcripts.
- Cold loads defer FTS, rollups, and read indexes in an unpublished sibling database;
  live batches rebuild each distinct affected usage bucket once, never once per message.
- Every `src_cursor` reconciliation predicate has a matching index. Query-plan tests pin
  these indexes because one missing provenance index turns source activation quadratic.
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
│   ├── store/                # SQLite projection, FTS, change log, rebuild   (L2 persistence, Phase 3)
│   ├── protocol/             # wire types (serde + ts-rs), versions          (L3 contract, Phase 4)
│   ├── server/               # axum REST + WS services                       (L3 runtime, Phase 4)
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
| FTS5 vs tantivy | FTS5 | relevance/latency benchmarks fail budgets |
| WS vs SSE for streams | WS (bidirectional subscribe/ack) | if consumers end up read-only-simple |
| Ledger retention | keep forever (no segment GC yet) | ledger > tens of GB |
| Grok/Codex priority | after M2 | chopsticks needs them sooner |
| UDS transport | not in v1 | localhost TCP shows up in profiles |
