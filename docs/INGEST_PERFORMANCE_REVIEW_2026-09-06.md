**Rust ingest performance review — 2026-09-06**

Yes, there is meaningful headroom. The clearest opportunities are shared-loop latency, using the existing faster cold topology consistently, and removing work that scales with unrelated sources or pending records. Replacing the JSON library is a later optimization.

This review covers capture, JSONL framing, ledger writing/replay, metadata admission, live scheduling, canonical application/reconciliation, cold publication, and detached search construction. Baseline measurements used a release build of `a66259c4c9958622a814fefe79d08294951a69ef`, Rust 1.97.1, macOS/arm64, and temporary files on the system volume. Protocol/server/query edits appeared concurrently near the end; they are outside that baseline. The numbered findings describe the original review; the implemented follow-ups below resolve findings 1 and 3 and reduce the detached-search cost in finding 7.

**Implemented follow-up — 2026-09-06**

All live `serve` paths and `watch` now share one bounded watcher wake-up and visit runners without waiting on each runtime. Exhausting a runner's hint budget requests another fair pass immediately; serve publishes its committed effects before visiting the next runtime. Queue signals survive the transition into a wait. A disconnected watcher requests recovery once and then uses the existing periodic polling lane.

Pending reconciliation now seeks matching targets directly in its ordered map, preserving evidence order without allocating a separate target index. A source/generation index bounds replacement invalidation to affected fragments. Append/Stage batches skip invalidation, and batches with no relations skip SQL reconciliation probes. Pending entries and source ownership change only after the transaction commits, including when a newer usage snapshot comes from another source.

Paired release probes show the improvement:

| Unmatched records in another session | 300 ordinary message appends, before | After |
|---:|---:|---:|
| 0 | 46.80 ms | 46.92 ms |
| 1,000 | 62.64 ms | 47.21 ms |
| 10,000 | 238.27 ms | 47.75 ms |
| 50,000 | 1,219.10 ms | 46.54 ms |

The largest backlog case improved **26.2×**. The original drift-only probe improved from 1,033.17 ms to 3.55 ms at 50,000 pending records. Nine idle runners went from **2,309.42 ms to 259.61 ms** per shared pass; one runner remained approximately 260 ms. These measurements isolate pending-backlog cost and scheduler waiting. Actual notification delivery and subscriber publication also pass dedicated correctness tests.

Each batch result is the median of three 300-batch rounds, excluding setup. The reconciliation pair uses frozen harnesses and the same Rust source tree except for `ram_reconcile.rs`; both executables are retained. Ordinary-message probes assert 900 canonical messages and zero drift after each backlog case. Whole-process peak RSS for that probe was 316.6 MiB before and 316.8 MiB after. Its orphans had no source stamp; source-owned orphans additionally require reverse-index keys. Idle comparisons exercise the old blocking and new shared-wait APIs with the same binary and timeout, with three repetitions.

The final workspace suite on the combined working tree passes **840 tests**, with one opt-in watcher test ignored by default; that real filesystem watcher test also passed separately. Regression coverage includes a ready runtime after eight quiet queues, subscriber delivery before eight subsequent quiet runtimes, budget exhaustion, polling/audits, late tool evidence, changed usage ownership, abandoned replacements, and checkpoint rollback. The stale Claude stream-count assertion was updated against its declared drivers. The multi-session slice comparison now maps local cursor IDs through source identity while retaining exact byte-offset and timeline comparisons. An isolated snapshot of this commit also passes the daemon subscriber-delivery regression against the unchanged protocol/server code.

A fresh ingest of the original 200 read-only source files preserved all checksums and matched all 17 normalized canonical table digests: 120,151 messages and 200 sessions. Search document/lane digests and ordered results for the same 12-query diagnostic suite also matched. The query suite and executable hashes are retained with the pair. This validates the shared store change on the frozen corpus; the follow-up's speed claims are the live probes above.

Strict workspace Clippy remains blocked by existing `large_enum_variant` diagnostics in unchanged `core` types; a changed-crate run also exposes the same existing issue in `store::DeferredOp`. These predate this patch. The full workspace passes Clippy with only that lint category excluded (`-D warnings -A clippy::large_enum_variant`). Formatting checks pass for every changed Rust file.

Follow-up evidence is in `/private/tmp/rosetta-ingest-review-20260906/`: `implementation-manifest.json`, `paired-performance.json`, `paired-{before,after}-{pending,pending-message}.jsonl`, `paired-idle.jsonl`, both `reconciliation-*-probe` binaries, their harnesses, `implementation-final-tests.log`, and `after-fix-validation/verification.json` with its checksummed query suite. Re-run the retained binaries with `pending` or `pending-message`; the after binary also supports `idle` and `idle-shared`.

**Cold-ingest follow-up — 2026-09-06**

Search construction now caches message/tool column positions per statement while retaining the same typed decoder and validation, and gives its private build connection a 16 MiB SQLite pager cache. On the same read-only 317 MB corpus, three alternating pairs against the retained `7e6b873` binary reduced full parallel-RAM ingest median **13.72 → 11.68 s (14.9%)** and search construction **8.17 → 5.71 s (30.2%)**. These are newly paired measurements; they are not compared against the older baseline timings below. One ordinary fresh-serve pair reached search-ready at **12.85 → 10.58 s** without a renderer.

All six cold builds, both serves, and a replay from the frozen baseline ledger match across 17 normalized canonical tables and all searchable document lanes. The required frozen `search-bench` comparison passes 16 diagnostic product-grammar cases, 100 measured cycles per shared/fresh backend lane, with identical ranked results. All **842 workspace tests pass**, with one opt-in watcher test ignored. See [the architecture performance record](ARCHITECTURE.md#71-cross-adapter-applicability-checkpoint-2026-08-27) for configuration, checksums, memory, and validation limits. Evidence is retained in `/private/tmp/rosetta-cold-ingest-20260906/`, including binaries, `cold-results.json`, `serve-results.json`, verification reports, and `search-validation/`; repeat databases are stored in verified lossless archives.

**Measured baseline**

The repository's deterministic generator produced 100 Claude Code sessions and 100 Codex sessions, each adapter contributing 100,000 lines, seed 42 and tool ratio 0.30. Total source size was 316,975,655 bytes. Source files were made read-only and all 200 SHA-256 checksums matched after the campaign. Each measurement created a new projection, ledger, and search sidecar. Three runs per configuration used rotating execution order. These are new-database runs with warm source/cache effects, not measurements after clearing the OS cache.

| Existing configuration | Full process wall, median | Observed wall range | Wall minus search, median* | Whole-process peak RSS, median |
|---|---:|---:|---:|---:|
| Ordinary `ingest`, serial/disk | 15.25 s | 14.41–15.55 s | 9.18 s | 1,159 MiB |
| Parallel capture, async ledger, disk projection | 11.98 s | 11.48–12.25 s | 5.35 s | 1,172 MiB |
| Parallel capture, async ledger, RAM projection | 10.73 s | 10.60–12.79 s | 4.53 s | 1,442 MiB |

*This column subtracts that run's measured detached-search elapsed time from external process wall. It includes other process overhead and is not an independently timestamped canonical-readiness measurement. Nested profiler counters were not added together. RSS includes search and teardown, so it cannot identify which allocation phase established the peak.*

On this corpus, the existing parallel RAM configuration reduced full-command median wall by 29.7%, and the derived wall excluding search by 50.7%. The disk parallel configuration reduced those measures by 21.5% and 41.7%. These compare existing execution configurations, not an implemented optimization or a before/after fresh-app campaign. Normal fresh `serve` already uses the parallel RAM topology. The measurements do not establish an additional 2× speedup for that app path or for the full organic nine-adapter corpus.

All nine projections passed `quick_check` and foreign-key checks. They contained 120,151 messages, 200 sessions, 5,485 file edits, 836 usage buckets, and zero drift rows; all runs reported zero hard parse errors and oversized records. Representative serial, parallel-disk, and parallel-RAM outputs had equal normalized content-table digests. A separate default ledger rebuild also matched. Comparison mapped surrogate message/cursor references to stable identities and excluded observation timestamps, projection epoch, source-audit rotation, and replay-specific source anchors. In the cold-to-cold comparison, the checkpoint JSON itself matched; the cursor difference was only `updated_at`, which prepared application explicitly writes as `None` at `pipeline/src/replay.rs:1538`.

The three representative search sidecars had identical search-document/lane digests and matching ordered results for 12 diagnostic MATCH queries, four of which returned hits. This is useful equivalence evidence, not a substitute for the product-weighted ranked-search campaign before changing search behavior.

**1. [P1] Idle runtimes delay live ingestion and subscriber delivery.**

In [the shared live loop](../crates/rosettad/src/serve.rs), lines 1433–1441 call `runner.tick()` sequentially and drain effects only after every runtime returns. [LiveRunner](../crates/pipeline/src/live.rs), lines 40–45 and 283–303, gives each runner its own 250 ms blocking first receive. The production receiver implements that timeout using `recv_timeout`.

An event queued for a later runtime can sit behind every earlier idle timeout. Even a commit processed early in the loop can wait behind the remaining idle runtimes before its WebSocket effects are delivered. A probe through the actual `LiveRunner::tick` API, with an event source honoring the quiet timeout, measured 252 ms for one runtime, 1,518 ms for six, and 2,275 ms for nine. This isolates scheduler waiting; it is not an OS-watcher-to-WebSocket latency measurement.

Use one shared wakeup for all runtime notifications, then drain each ready runtime without a blocking timeout. Keep the canonical writer single-owner and publish effects promptly after commits. Verify an active runtime among eight idle runtimes, including effect-delivery latency, before considering smaller parser changes. The legacy serve loop has the same per-runtime blocking pattern.

**2. [P2] The ordinary CLI and ingest workbench do not exercise the faster app topology.**

[Command dispatch](../crates/rosettad/src/main.rs), lines 1247–1266, selects parallel cold capture only with `--experiment-parallel-cold`. The default serial branch also walks roots again through `meta_ingest_into`, even though discovery already returned the inventory. [The workbench](../tools/ingest-bench/src/lib/benchmark.js), lines 135–141, invokes ordinary `ingest --profile --json` without those topology flags.

The faster configuration already has the essential machinery: inventory reuse, concurrent runtime producers, async raw-ledger writers, prepared ordinals, and closed source application. The serial runs paid a median 1.46 s for global renumbering; prepared runs paid zero in that timer because they computed ordinals beforehand. Preparation still costs CPU, and is part of the measured process wall.

Consolidate fresh finite ingest and fresh serve around shared production cold-building code, retaining their distinct publication/read contracts. Give the benchmark an explicit topology field and a production-path option. Preserve existing-projection live behavior, ledger durability, validation, and failure recovery. Do not promote the broader collection of experimental flags wholesale.

**3. [P2] Unmatched records slow unrelated live batches.**

[RAM reconciliation](../crates/store/src/apply/ram_reconcile.rs), lines 193–206, traverses the entire pending map for every source-bound batch, including Append/Stage cases whose invalidation predicate is always false. Lines 217–227 traverse it again to look for matching openers, including batches with no openers. `PendingKey::target()` also allocates cloned target strings during this scan. Cost therefore grows with total pending state, rather than the batch's affected targets.

The following release probe seeded unresolved usage records for session A, then applied 300 distinct drift append batches to unrelated session B in an in-memory store. Setup was outside timing; values are medians of three repetitions. It uses production store/reconciliation code and omits ledger, filesystem, and subscriber costs.

| Unresolved records in A | Time for 300 unrelated batches | Time per batch |
|---:|---:|---:|
| 0 | 3.53 ms | 11.8 µs |
| 1,000 | 16.25 ms | 54.2 µs |
| 10,000 | 168.24 ms | 560.8 µs |
| 50,000 | 1,112.89 ms | 3.71 ms |

First skip invalidation unless the source mode can invalidate, and skip draining when the opener set is empty. Then index pending entries by relation target and source/generation, so replacement cleanup and late-opener lookup touch only the relevant entries. Preserve evidence ordering, replacement invalidation, and commit/rollback behavior. The large backlog is a stress case, not a claim about the user's current unmatched-record count.

**4. [P2] The fallback poll performs a quadratic roster search.**

[LiveRunner::poll](../crates/pipeline/src/live.rs), lines 414–427, calls `self.files.iter_mut().find(...)` for each walked file, then performs a cursor lookup and checkpoint decode. A quiet poll over N JSONL sources performs approximately N²/2 path comparisons. Hint mapping also repeatedly scans the roster.

A probe generated and fully ingested separate fixture corpora, then timed forced quiet catch-up polls. No records streamed during the measured polls. Median times were 9.12 ms for 1,000 files, 71.43 ms for 5,000, and 212.44 ms for 10,000. Those measurements include the walk and cursor work, not just comparisons. Growth was over 23× for a 10× larger roster.

Maintain a path-to-roster index plus a physical-snapshot-to-members index, and consider fetching the runtime's cursor control rows in one query per poll. Retain cursor-relative evidence, stale-normalizer detection, directory hints, and new-source admission. Avoid substituting process-local stat history for the durable cursor checks.

**5. [P2] Full-file audits can occupy the only writer, outside the read timer.**

[The audit branch](../crates/pipeline/src/stream.rs), line 546, calls `verify_committed` synchronously before entering the instrumented ordinary read. [Verification](../crates/capture/src/tailer.rs) reads and hashes the entire committed prefix without a byte/time budget or cancellation check inside its loop. An audit allowance of one file does not bound latency when that file is large. Its bytes are counted, but its elapsed verification time is absent from `ReadNanos`.

Add an explicit audit timer and byte/time scheduling budget. Consider resumable verification or a worker whose result is validated against the source identity/generation before application. The correctness requirement remains detection of stamp-preserving rewrites.

The current resumable FNV chain is also a serial per-byte dependency in normal framing. A hashing-only control over 512 MiB measured 716 ms for the existing chain and 307 ms for BLAKE3. This is not a drop-in replacement or an end-to-end saving: checkpoint compatibility and streaming resume semantics differ. A versioned block-based chain and faster delimiter framing merit a later experiment. `memchr` supplies optimized byte-search routines, but it does not remove the cost of hashing every byte. [memchr documentation](https://docs.rs/memchr/latest/memchr/)

**6. [P2] Whole-runtime ownership and reconciliation copies are the next cold CPU/memory target.**

[Planning application](../crates/store/src/lib.rs), line 542, clones each complete batch into `planned_batches`, retaining normalized payload for an entire runtime. Prepared ordinal assignment builds multiple String-keyed trees and repeats session/path/timeline strings. Closed application retains source batches while constructing reconciliation deltas; [deferred-op fingerprints](../crates/store/src/apply/bulk_reconcile.rs), line 119, serialize each deferred operation to a temporary JSON buffer before hashing, alongside cloned deferred payloads. Generation cleanup also uses full-map `retain` calls.

In the parallel RAM runs, median outer `apply time` was 2.72 s while `canonical apply time` was 1.02 s. That difference is not a measured allocation subtotal: it includes several kinds of pre/post-transaction work. It does show why SQL-statement timers alone cannot identify the whole apply bottleneck.

Start with ownership transfer into planners, compact/interned identity keys, range-scoped source cleanup, and explicit allocation/fingerprint/delta timers. Investigate streaming serialization into a hasher where fingerprints are necessary. Runtime-wide accumulation also means one dominant adapter does not get file-level normalization parallelism. Partition only after proving that every cross-file correlation and replacement boundary closes within a group; independent session or correlation groups could then reduce peak memory and increase parallelism. An arbitrary per-file split would change correctness.

SQLite snapshot sources deserve their own memory campaign: `capture/src/sqlite.rs::query_rows` materializes all rows as JSON strings, then `pipeline/src/snapshot.rs:186–193` wraps/copies them into another per-source collection before hashing and parsing. A 512-op apply chunk does not bound that raw snapshot allocation. The JSONL measurements above say nothing about large Hermes/OpenCode databases.

**7. Detached search is now the largest measured end-to-end target.**

The parallel RAM configuration spent a median 6.20 s in detached search, versus 10.73 s full process wall. [Search generation construction](../crates/search/src/worker.rs), lines 135–215, combines canonical document extraction, asset expansion, metadata inserts, FTS inserts/merges, index construction, integrity checking, and commit into this broad stage.

Add separate timings for those subphases before choosing an optimization. Then benchmark private-build pager/cache settings and FTS merge policy against the actual expensive component, retaining exact search lanes, ranked results, integrity checks, and publication semantics. Prepared inserts, one bulk transaction, and deferred metadata indexes already exist; recommending those generically would add nothing. FTS5 exposes merge controls, but a different setting is an experiment, not an established win. [SQLite FTS5 documentation](https://sqlite.org/fts5.html)

Keep canonical readiness and search readiness separate. The app already does this; the finite CLI waits for both. A faster canonical pipeline alone cannot remove the search portion of this command's wall time.

**8. Replay still reads the ledger three times, but removing validation is not the fix.**

[Rebuild preflight](../crates/rosettad/src/main.rs), lines 1767–1783, performs a full checksummed scan. The ordinary replay path then elects generations with another scan and rereads to normalize. With compressed segments, those are repeated decompression/checksum passes.

A single diagnostic rebuild of the same frozen ledger took 15.63 s including search. Preflight was 0.658 s; the election read timer was 0.632 s and replay read timer 0.699 s. The sum of those three distinct read scopes was about 1.99 s. This was one run, not a paired optimization campaign.

Investigate combining verified preflight with election or a writer-produced authenticated control summary. The current best-effort `.segments` index is not a substitute for integrity validation. Earlier payload-light election experiments were already rejected in the architecture's performance history; do not revive them solely because a timer looks removable.

**9. JSON allocation can improve, with a modest end-to-end ceiling here.**

Both major JSONL adapters first deserialize a complete `serde_json::Value`, then construct typed records from it: [Claude parsing](../crates/adapters/claude-code/src/raw/transcript.rs), lines 38–56, and [Codex parsing](../crates/adapters/codex/src/raw/rollout.rs), lines 34–53. This is a full DOM followed by typed conversion, not two text parses.

A known-record-only Claude probe compared the existing parser with direct typed deserialization. It validated equal serialized typed records for 20,000 selected generated lines, then parsed those lines five times per sample. Median time for 100,000 parses fell from 501 ms to 342 ms, a 31.7% parser-only reduction. Unknown-record dispatch, malformed-input behavior, drift fidelity, and complete adapter normalization were outside that comparison. Therefore this is a candidate, not a parser replacement ready to ship.

Consider borrowed/tagged envelopes and typed visitors, preserving permissive unknown fields and drift. Borrowed `RawValue` can defer materialization of selected subtrees where semantics permit it. [serde_json documentation](https://docs.rs/serde_json/latest/serde_json/value/struct.RawValue.html) Complete normalization cost about 1.31 s of cumulative worker time in the parallel RAM configuration and overlapped other work. No large whole-engine speedup follows automatically from the parser microbenchmark.

**Measurement and validation cautions**

The profiler needs execution-mode-aware labels. Parallel runs reported only about 34 ms of `ledger write time` even though worker framing, buffered writes, and flushes each took hundreds of milliseconds: caller enqueue/join time and worker execution time are different quantities. Likewise, default rebuild reported zero `lines parsed` in the generic Instrument report while its per-runtime rebuild reports counted the replayed records. The emitted `parallel-stream-prepare` wall is captured before `prepare_captured_runtime` runs, so that stage name also hides ordinal-preparation work; only the larger capture wall includes it. Add queue-wait/worker-busy, audit, preparation/ordinal, and search-subphase timers; keep process wall authoritative.

The old August 31 physical-floor document predates the current compressed ledger. In this campaign, approximately 323.6 MB of frame data occupied 45.4 MB on disk. Its older uncompressed “near the I/O floor” argument should not be reused as a lower bound for today's implementation. Retain its historical observations, and rerun the controls under the current representation before setting a new throughput ceiling.

Release tests covered core, capture, store, pipeline, search, daemon, all nine adapters, and corpus generation. One baseline contract test is stale: `rosettad/tests/adapter_optimization_matrix.rs:180` expects Claude `(2 JSONL, 1 document, 0 SQLite)`, but the current adapter admits `(3, 2, 0)`, including Workflow journal/run streams. The unchanged test fails at line 286. This leaves the advertised cross-adapter gate red and should be corrected against ground truth before merging a performance change.

Five fresh-serve tests initially could not bind localhost inside the sandbox. All five passed when their original compiled test executable was rerun with local-network access, including warm append through SQLite, ledger, and WebSocket, and interrupted-cold recovery. Across the completed targets, counting duplicate reruns once and replacing the sandbox failures with that successful rerun, **764 tests passed, one failed, and one was ignored**. This is the tested ingest-related scope, not a full-workspace green claim. A later attempt to rebuild the concurrently edited protocol/server tree encountered compile errors; those are not attributed to the reviewed ingest baseline.

**Suggested implementation order**

1. **Done:** fix the shared live wait/effect-drain boundary and index pending reconciliation.
2. Add indexed poll/hint lookup and bounded, timed audit scheduling.
3. Make the benchmark select and label the production cold topology; unify finite cold building with the existing fast implementation after its compatibility gates pass.
4. Profile and reduce cold ownership/fingerprint overhead, and decompose detached-search construction.
5. Pursue parser, framing/hash, and replay-scan changes with frozen comparisons and their specific compatibility tests.

Keep single-writer canonical transactions, raw-before-visible durability, generation-safe replacement, deferred cold indexes/rollups, atomic publication, and permissive adapter semantics. Previous failed batch-size, insert-first, and SQL folding experiments are documented in `docs/ARCHITECTURE.md`; they should inform new hypotheses rather than be repeated as generic advice.

**Reproduction and retained evidence**

Local evidence lives at `/private/tmp/rosetta-ingest-review-20260906/`: `manifest.json`, `benchmark-results.json`, `summary.json`, `integrity.json`, `semantic-validation-final.json`, `search-semantics.json`, individual run events, probe source/results, and test logs. The release binary was built in `/private/tmp/rosetta-ingest-review-20260906-target/`. Its SHA-256 was `63828c93b9e727e2e1acd96dd2bce78a46d973b89e179523ab4795bae2fe5b76`.

The ordinary baseline command was:

```sh
rosettad ingest --home CORPUS_HOME --agent claude-code --agent codex \
  --db NEW_RUN/projection.sqlite --json --profile
```

The parallel-disk variant added `--experiment-parallel-cold --experiment-cold-completion-order --experiment-async-ledger`; parallel RAM also added `--experiment-sqlite-in-memory`. No variant disabled the raw ledger, search, checks, or final durability. The generator commands were `bench-corpus gen claude-code --sessions 100 --lines 100k --seed 42 --out CORPUS_HOME/.claude` and the equivalent `codex` command targeting `.codex`. Record process wall and per-child peak RSS independently; exclude generation/hash/setup time; use a new output directory for every run.
