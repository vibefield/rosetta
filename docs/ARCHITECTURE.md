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

> **Implementation checkpoint (2026-08-27).** This document describes the target
> architecture. Nine ground-truth/runtime-adapter packs are registered. The generic
> runtime handles adapter-selected JSONL and SQLite sources, generation-staged
> reconciliation, deterministic adapter checkpoints, FTS and rollups, live watching
> with polling/audit recovery, a checksummed append-only raw ledger, and validated
> whole-projection rebuild with atomic installation. Fresh backfill and explicit rebuild
> use a private projection: FTS, usage buckets, and read-only indexes are built once,
> validated, and atomically published. Live batches coalesce repairs by
> `(session, hour, model)`, and generation sweeps use indexed provenance. `source_op` is temporary
> projection-internal rewrite staging, not a third durable representation. A source's first
> generation projects directly inside the unpublished cold database; only replacements of an
> already-active generation need staging. The store hot tier, in-process subscription
> hub, axum REST/WebSocket transport, backend-neutral wire types, generated TypeScript
> package, and `SearchBackend` seam with synchronous FTS5 as its reference are implemented.
> Consumer migrations, a detached search worker, and the dedicated metrics stream remain.
> Unknown projection schemas fail closed on ordinary open and are replaced only through
> the explicit ledger rebuild command.
>
> Organic Claude Code, Codex, and Grok validation on 2026-08-26 established the first
> measured performance baseline. On the final Codex cold run, FTS5 construction was
> 33.85 s of 101.23 s, canonical application 24.08 s, and ledger writing 16.36 s. A
> frozen-ledger rebuild took 79.53 s, led by FTS5 at 24.83 s. The evidence, readiness
> split, crash-safe sidecar protocol, and ledger/writer optimization gates are specified
> in [`docs/ingest-engine.html`](ingest-engine.html) under “Measured performance and
> search-finalization specification.” Tantivy 0.26.1 is now an optional benchmark-only
> dependency, not a selected production backend or canonical store.
>
> A newer frozen-source campaign covers the six locally installed adapters from one
> checksummed, permission-read-only snapshot: 181,526 source files, 38,998 matched files,
> 2,234,095 records, 6,414 sessions, and 773,375 messages. The `44e8354` cold baseline
> published in 451.54 s. The crash-safe staged-ledger path at `f22bd63` published the
> exact same logical and ranked-search result in 352.52 s (−99.02 s / −21.9%): ledger
> write time fell from 96.64 s to 18.02 s, segment syncs from 7,435 to 66, and segment
> sync time from 81.14 s to 0.13 s. Twelve required directory barriers added 0.0005 s.
> The same ledger rebuilt in 253.54 s. Remaining cold costs are canonical application
> (102.64 s in the hardened run), synchronous FTS5 (84.99 s), global ordinal
> finalization (36.75 s), source read (22.10 s), normalization (16.52 s), and usage
> buckets (15.06 s); these phases vary materially between otherwise equal runs.
> Diagnostic broad-ranked FTS p95 is 193.99–196.59 ms while selective-ranked p95 is
> 10.11–10.91 ms. The bounded Tantivy bake-off on the retained 768,280-message frozen
> projection built in 11.285–11.330 s and returned aggregate p95/p99 in
> 9.669–9.688/10.102–10.344 ms with zero filter violations and stable ordered results across
> independent builds. Its 1.529–1.531 GiB index is 2.26× the 0.677 GiB SQLite FTS structures,
> and top-100 overlap with FTS ranges from 7% to 80%; without judged relevance or product
> weighting, production adoption is deliberately deferred.
> SQLite remains the canonical transactional projection.

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
`GET /api/search?q=&runtime=&project=&session=&after_ts=&before_ts=&kind=` → FTS5 match
with snippets and rank, with structured filters applied inside the ranked query.

**Observe (job 2)** — the flagship contract, successor to `observeSession`:

```
WS /ws/sessions/<id>?resume=<stamp>
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

`GET /api/runtimes`, `/api/projects`, `/api/sessions`, and `/api/sessions/{id}` serve
the metadata/browse side of the agent-view app; `/ws/meta` streams its changes.

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
  batched transactions. Today, one live ingest transaction applies canonical facts,
  FTS maintenance, rollup corrections, and checkpoint/reconciliation state. The target
  search split moves only disposable search maintenance behind a durable repair set;
  canonical rows, rollups, and source-generation correctness remain atomic in SQLite.
- **FTS5 external-content today**, behind an implemented backend-neutral
  search/readiness seam. Organic broad-term ranking averaged roughly
  119–143 ms on the large Codex/Claude projections, while selective retrieval remained
  sub-millisecond. The disposable Tantivy prototype clears the frozen latency, build,
  filter, and deterministic-order gates but not relevance, product-mix, storage-margin,
  live-overhead, or crash gates. Retain FTS5 as the shipped reference and defer production
  sidecar work until those inputs justify it; SQLite remains canonical either way.
- **On-disk layout:** `<projection>.ledger/<agent>/` (segments) + the configured SQLite
  projection; `--ledger` can relocate the segment root. An adopted search backend owns an
  optional disposable `<projection>.search/`. Ledger is precious; DB and search indexes
  are rebuildable projections.
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
| Antigravity | 1 JSONL | yes | none observed | split at every fixture line |
| Claude Code | 2 JSONL streams | yes | inline on messages | split at every fixture line |
| Codex | 1 JSONL | yes | separate `Usage` merge | pipeline split/resume projection equality |
| Copilot | 1 JSONL | yes | inline + separate `Usage` merge | split/resume state test |
| Grok | 1 JSONL | yes | separate `Usage` merge | checkpoint/one-shot equality |
| Hermes | 1 SQLite snapshot | yes | none observed | stateless snapshot replay equality |
| Kimi Code | 1 JSONL | yes | inline on messages | split/resume state test |
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
canonical semantics.

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
- Batch session timestamp touches and target-aware pending reconciliation are
  roster-wide: all nine adapters exercise both `Message` and `ToolResult` store paths.
  The transaction-local gate caches exact usage/result targets by canonical session,
  loads them through the existing primary key, and updates immediately on queue/drain.
  Persistent orphans in one session therefore remain recoverable without poisoning
  unrelated messages, sessions, or adapters.
- Same-batch tool-result folding is roster-wide in semantics, but the measured SQLite
  implementation is rejected. On the frozen mixed ledger it safely folded 358,790 of
  360,052 tool results plus 149,755 of 151,298 separate usage snapshots (99.45% overall)
  and retained exact logical counts. Fully populated message inserts nevertheless raised
  message-write time from 9.327 s to 17.113 s and canonical apply from 36.923 s to
  42.805 s (+15.9%). The candidate was reverted at `0d5796d`; cross-batch, cross-file,
  duplicate-target, and pending reconciliation remain on the established ordered path.
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

The applicability is general; unpaired saving estimates are not. Always-on split counters
now attribute replay cost by adapter and canonical statement family. The target-aware gate
at `8fc0b50` reduced pending gate-plus-drain SQL from 7.905 s to 0.424 s on the same frozen
ledger: 785,985 of 786,004 message probes were skipped and the 19 exact matches still
drained. Exact logical/ranked-result equivalence passed. Wall moved from 317.49 s to
252.02 s, but that is not the attributed saving because FTS and validation varied by
30.292 s; the defensible pending reduction is 7.481 s (94.6%).

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
- Cold loads project each source's first generation directly and defer presentation
  renumbering, FTS, rollups, and read indexes in an unpublished sibling database. One
  set-based finalizer establishes canonical ordinals before validation/publication; live
  batches and replacement swaps remain transactionally canonical. Live batches rebuild
  each distinct affected usage bucket once, never once per message.
- A first cold projection now requires both the projection and ledger destination to be
  absent. It buffer-flushes into a same-filesystem private ledger tree, syncs every staged
  segment plus child-to-parent directory entries, atomically promotes the whole tree,
  syncs its parent, and only then installs the validated SQLite file. A missing projection
  beside an existing ledger fails closed to `rebuild`, preventing generation collisions.
  Existing/live projections retain ledger-sync-before-visible-SQLite ordering. The frozen
  pair measured segment syncs at 7,435 → 66 and ledger time at 96.64 s → 18.02 s; twelve
  directory barriers cost 0.0005 s. Search is independently disposable and may never
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
│   ├── store/                # SQLite projection, FTS, change log, rebuild   (L2 persistence, Phase 3)
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
| FTS5 vs Tantivy | synchronous FTS5 today; prototype is fast but production adoption is deferred | revisit with judged relevance + declared/observed query mix + accepted storage margin; then require live-overhead and crash gates |
| WS vs SSE for streams | WS (bidirectional subscribe/ack) | if consumers end up read-only-simple |
| Ledger retention | keep forever (no segment GC yet) | ledger > tens of GB |
| Grok/Codex priority | after M2 | chopsticks needs them sooner |
| UDS transport | not in v1 | localhost TCP shows up in profiles |
