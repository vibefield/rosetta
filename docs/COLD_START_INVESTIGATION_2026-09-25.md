# Cold-start investigation — 2026-09-25

Latest full live-history remeasurement: **Claude Code 622.96 s (10m 23s), Codex 73.57 s (1m 14s)**, both canonical and search ready, using the release with deferred compression. See [the latest full-history repeat](#full-history-remeasurement-after-deferred-compression). Earlier sections retain historical measurements and their different inputs and host conditions.

Deferred-compression follow-up: normal cold serve now postpones compression until canonical/search readiness, with one worker targeting 10% of one core. Frozen before/after timings, measured CPU use, interruption/restart verification and data equality are recorded in [the deferred-compression section](#deferred-ledger-compression-after-readiness).

Initial reproduction: the then-current build completed; the pre-change comparison was deliberately stopped after approximately eight minutes, with canonical readiness still false, to release its memory on the saturated host.

Pricing follow-up: separate full Claude Code and Codex cold starts both completed after the pricing-read cleanup. Their stage measurements and limitations are recorded in [the separate-agent section](#separate-full-agent-cold-starts). These are different runs from the earlier combined nine-runtime reproduction.

## Findings supported by the completed current-build run

The reproduced bottleneck is host memory/CPU saturation, amplified by Rosetta's whole-runtime RAM capture/application pipeline. This is not evidence of a sixfold increase in source volume. The retained September 12 release also remained incomplete after approximately eight minutes on the same source roots. The recent usage changes therefore do not explain the slowdown by themselves. A residual code regression cannot be quantified from these varying-load, non-frozen runs.

The desktop app was running the release ARM64 daemon, protocol 23 / schema 24, against a fresh `manual-20260925-100715` projection on SamsungRed. It used ordinary `serve`, which already selects the parallel RAM cold path. It was not accidentally using a debug build or the slower serial CLI ingest path.

An isolated run of that same executable and the same nine native agent roots, without a renderer attached, reached canonical readiness in **521.50 seconds**. Metadata was public in **16.22 seconds**. The process exited successfully after 570.60 seconds. These timings include diagnostic sampling and VM inspections on a busy host; they are diagnostic observations, not an uncontended performance benchmark. `serve --ticks 0` does not guarantee a completed search index, so process exit is not a search-readiness measurement.

| Boundary | Current release |
|---|---:|
| Metadata ready, elapsed | 16.22 s |
| Codex stream, worker wall | 133.16 s |
| Claude stream, worker wall | 150.92 s |
| Codex canonical application, wall | 76.62 s |
| Claude canonical application, wall | 243.13 s |
| Canonical ready, elapsed | 521.50 s |

The two stream phases run concurrently; the canonical writer applies runtimes serially. Do not add the worker durations to reconstruct elapsed time. The profiler's inner timers also overlap their enclosing timers.

## Memory evidence and responsible code

- A native sample observed **10.2 GiB peak physical footprint** during Claude application.
- A VM inspection reported **7.4 GiB of writable regions swapped out**, approximately 9 MiB resident, and about **21 million heap allocations**. macOS's accounting includes memory held in its compressor; this is not a claim that all those bytes were being read from the swap file.
- Earlier system counters already showed a large compressed working set. During the run the Mac had approximately **28 GiB of swap in use**. Two one-second VM samples recorded 126,608–139,399 decompressions and 135,674–141,484 compressions of 16 KiB pages: approximately **1.9–2.2 GiB/s in each direction**.
- The system was busy independently of the benchmark: approximately 681% aggregate process CPU on ten cores. A later `top` snapshot during the old-build run showed **63 GiB used, 30 GiB in the compressor, 70 MiB unused, 0.35% CPU idle, 242 running processes, and load averages 543.69 / 478.86 / 392.64**. Its largest reported process footprints were Ghostty **17 GiB**, `fseventsd` **16 GiB**, and the existing Rosetta desktop renderer (PID 36846) **12 GiB**, including about **9.6 GiB compressed**. The isolated daemon then accounted for about 4.2 GiB. No thermal or CPU power warning was reported. These are observed footprints, not proof of a leak or attribution of `fseventsd`'s growth to a particular application.
- The late Claude sample spent essentially the entire sampled main-thread interval in `BulkReconciliationBuffer::committed`, releasing deferred tool/message trees. That is work on retained intermediate objects after their database transaction, not reading additional source files or calculating prices.

The relevant ownership path is:

1. [Fresh serve](../crates/rosettad/src/serve.rs) starts every runtime producer, each retaining its complete prepared runtime until the sole writer admits it.
2. [The planning store](../crates/store/src/lib.rs) clones each source batch into `planned_batches` (`apply_planning_source`).
3. [Bulk reconciliation](../crates/store/src/apply/bulk_reconcile.rs) clones tool rows, updates, and usage operations into deferred maps, serializes deferred operations again to fingerprint them, and constructs another reconciliation plan.
4. The completed plan and retained source trees are released after commit. Paging compressed object graphs back into memory just to traverse/free them becomes expensive on this host.

This pattern predates the September usage changes and was already identified as an optimization target in [the September 6 review, finding 6](INGEST_PERFORMANCE_REVIEW_2026-09-06.md). The current measurements establish that it is now a practical bottleneck on the organic corpus.

There is also an instrumentation gap: `Store::flush_bulk_reconciliation` times plan preparation and SQL/commit separately, but its subsequent `committed(&plan)` cleanup and plan destruction have no dedicated timer. They are included in the enclosing `Instrument::ApplyNanos`. An unexplained difference between outer apply time and inner SQL counters must not be described as database time.

## Are we ingesting unnecessary data?

The closest retained pre-change organic projection inspected here is September 11, not an exact snapshot from September 18. Cursor offsets provide historical bytes actually consumed, rather than today's file sizes. Queries against the running September 25 database used read-only SQLite connections.

| Stored/consumed quantity | September 11 | September 25 | Change |
|---|---:|---:|---:|
| JSONL bytes consumed | 11.903 GB | 14.144 GB | +18.8% |
| Sessions | 6,608 | 6,946 | +5.1% |
| Canonical messages | 581,365 | 642,568 | +10.5% |
| SQLite file | 1.389 GB | 1.656 GB | +19.2% |
| Cursor checkpoint text | 82.27 MB | 94.09 MB | +14.4% |

Decimal GB/MB are used in this table. The current live projection can grow after observation.

Claude transcript bytes grew from approximately **4.14 GB to 6.07 GB**; Codex grew from **6.72 GB to 7.06 GB**. Those two account for about 93% of the current source bytes. The largest Codex source was already approximately 852 MB in the old projection, so that file is not a newly introduced ingestion scope.

The **229 hidden Codex review transcripts total approximately 292 MB**, about 2.1% of all consumed JSONL bytes. They already existed as a ledger-only category in the older build (196 files / 337 MB). The new usage extraction does parse their usage, but their volume cannot explain the observed slowdown. The new Antigravity conversation and Copilot usage sources are tiny on this machine; all of Antigravity/Copilot combined processed about 1.14 MB in the original app run.

The normalizers already omit bookkeeping records from the canonical timeline. The renderer's global statistics requests still wait for `canonical_ready` and use four workers (`useProjectStats`); the earlier unbounded cold-query fan-out is not present in this code. Raw records are deliberately retained in the lossless replay ledger, including records that emit no visible message. The reproduction captured approximately **14.26 GB of framed ledger data into 3.38 GB of compressed ledger files**. That is the documented recovery/rebuild policy, not evidence that cache directories or arbitrary project files are being imported as transcripts.

The demonstrated excess is **copies and lifetime of parsed intermediate data in RAM**. Dropping historical sessions, subagents, usage facts, or raw recovery evidence would change product behavior and is not justified by this measurement.

## USD calculation timing

The list-price calculator was introduced in `8cce783` (September 22), with follow-up changes on September 23 and 24. It uses a compiled local catalog and stored token counts; the calculation itself performs no network or file I/O and does not ingest additional native history.

- **Cold finalization:** `Store::finish_bulk_load` calls `bulk::finalize`, which folds all stored `message` and `session_usage` rows into hourly `usage_bucket` totals before marking the projection canonical-ready. Rows without reported usage skip the pricer. Cold writes defer bucket maintenance, and the private bulk path suppresses the live mutation effects that would otherwise calculate individual row costs.
- **Opening an existing completed projection:** `pricing::ensure_current` synchronously rebuilds all usage buckets only if the stored pricing version differs from the compiled catalog/calculator version. On the ordinary warm serve path this happens before the HTTP listener binds. Matching versions skip the rebuild; a newly finalized projection already carries the matching version. This uses stored facts, without replaying the ledger.
- **Live ingestion:** each transaction rebuilds the affected session/hour/model buckets. Each rebuild prices the usage records in those buckets, including existing records. Watched message/usage mutations also compute the row cost for delivery to clients.
- **Reading rows (before the follow-up fix):** `message_codec::decode_columns` calculated an individual row's cost from its stored usage whenever it decoded a timeline entry. Transcript requests need this value. The search document visitor also used this decoder, calculated the cost, then discarded it because `SearchDocument` has no cost field. This was unnecessary pricing work during initial search construction and subsequent search repair. The detached search build starts after canonical readiness.

The completed current-run profile reports **9,271.8 ms for the entire final bucket fold**, including SQL scanning, token aggregation, grouping, and USD calculation: approximately **1.8% of the 521.50-second canonical-ready interval**. This is not an isolated price-only measurement and does not include read-time pricing in search or client requests. It does establish that the full-history price rollup was not the dominant cold-start stage in this run. The extra search pricing has not been timed separately. Aggregate UI requests read the stored bucket costs, while the renderer sums and formats the returned USD values.

The follow-up change makes pricing explicit for transcript queries through `decode_priced`. General decoding retains stored facts and validation, while search, task/plan metadata, file targets and Workflow attempt summaries skip USD calculation. Accurate full-history costs cannot be supplied by the bounded metadata skim: they depend on streamed usage and cross-file reconciliation. The readiness rationale is recorded in [ARCHITECTURE §4.1.3](ARCHITECTURE.md#413-usage-accounting-and-list-price-usd-2026-09-22).

Code: [bulk finalization](../crates/store/src/bulk.rs), [pricing/version check](../crates/store/src/pricing.rs), [row decoder](../crates/store/src/message_codec.rs), [search document visitor](../crates/store/src/queries.rs), [calculator](../crates/pricing/src/lib.rs).

## Remediation priorities

1. Establish an uncontended repeat after reducing the host's existing memory load. Investigate the 12 GiB desktop renderer independently; restarting/closing memory-heavy apps or restarting the machine is an operational recovery option, not a measured code fix. No existing app or system service was stopped here. Keep the same binary, source cut, and output device; record canonical and search readiness separately. Do not use the 2-minute recollection as a measured baseline.
2. Reduce peak cold-build memory: consume owned batches where possible, share/move large tool payloads instead of cloning them across reconciliation representations, release input containers at their last use, and budget concurrent producer memory. Preserve the closed-runtime publication and raw-ledger recovery guarantees. Arbitrary per-file publication is not safe for cross-file correlations.
3. Add explicit preparation, reconciliation cleanup/destruction, and peak-memory measurements. The current SQL counters conceal the cost of releasing large intermediate trees.
4. Use a stable completed projection for ordinary app starts. `AH_DB` pointing at a new timestamped directory deliberately forces the full history to be rebuilt; ordinary warm startup already follows a separate cursor-relative catch-up path.

## Evidence and comparison

Diagnostic evidence is local, outside version control, under `/private/tmp/rosetta-cold-regression-20260925/`. They include the original status snapshot, historical database/volume summaries, executable hashes, timestamped stdout, stage profiles, process/VM observations, and native samples.

The original app database and native agent sources were not modified by the investigation. Reproductions used separate, newly created benchmark directories on SamsungRed. Their disposable databases, ledgers, assets, and search scratch are removed after measurements; `cleanup.json` records the reclaimed sizes. The diagnostic evidence listed above is retained.

The current executable SHA-256 is `c13851b8207e3e8b6ffa5838811ef1dab4984faa24b003e0118149099b9670f2`. The retained September 12 release SHA-256 is `e4439d5a9e72502a96ede19f786e1d09fa3388bfde6cdcd89d038644a314d469`; its ready event reports protocol 20. Its file date/protocol establish a pre-usage-change binary, but the executable does not expose an exact source commit. Both runs use fresh `serve --all-agents --ticks 0 --json --profile` projections and execute sequentially. Native sources remain live, and host load varies, so this is a diagnostic comparison rather than a frozen, repeated optimization campaign.


The old-build comparison was stopped with SIGTERM after 478.97 seconds (480.44 seconds through process exit). This was an investigator cancellation, not a daemon failure. Its metadata milestone was 15.74 seconds, and its Claude streaming worker took **189.02 seconds**, compared with **150.92 seconds** for the current release. At the last successful status observation, Claude was still Applying and Codex was Preparing, after both had consumed their source payloads. This is a lower bound on its readiness time, not a completed cold-start timing.

The dominant source payloads differed by less than 0.1% between these two runs: current Claude/Codex processed 6,075,303,262 / 7,063,479,946 bytes; the pre-change build processed 6,076,614,750 / 7,065,271,064 bytes. Source growth during the comparison cannot account for the older build's multi-minute delay. The old run received lightweight status/VM-counter monitoring, with no process VM-map inspection. The current run had additional native stack samples and VM-map inspections; two status samples timed out during a VM inspection. Consequently no percentage code speedup/slowdown should be inferred from these runs.

## Separate full-agent cold starts

Campaign `rosetta-agent-stage-profile-20260925` ran the complete discovered Claude Code history, then the complete discovered Codex history, sequentially. Each used a fresh database/ledger and the normal production RAM `serve` path, without a renderer. The current working-tree release includes the pricing-read cleanup and added attribution timers. Both executions used binary SHA-256 `fc4629beba50dabebf4752e59d523667e2a99496aed02b72c4af9bf0cd7cf238`.

The machine was the same Apple M1 Max / 10 cores / 64 GiB, with outputs on SamsungRed. Source histories remained live and OS file caches were not cleared. Release compilation took 8m 23s before the runs and is excluded. Each daemon was stopped gracefully only after HTTP status confirmed both canonical and search readiness; both exited successfully.

### Elapsed stage times

Seconds. The exclusive phase rows sum to canonical readiness; the additional post-publication interval takes that to full readiness. Stage durations come from native timers. Readiness totals start at process launch, including executable startup and observation latency in the residual row. Search readiness was polled approximately every 250 ms after canonical publication.

| Stage | Claude Code | Codex |
|---|---:|---:|
| Discovery | 3.22 | 0.35 |
| Metadata ingestion | 6.09 | 0.57 |
| Source reading, parsing, normalization and ledger capture | 60.93 | 40.20 |
| Closed source-plan preparation | 3.95 | 0.89 |
| Source application and reconciliation | 49.22 | 16.05 |
| Finalization: rollups, indexes, validation | 7.74 | 2.51 |
| Durable publication | 6.71 | 0.96 |
| Release canonical RAM and reopen published database | 0.17 | 0.04 |
| Other startup/coordination and observation | 1.37 | 0.14 |
| **Canonical ready, elapsed** | **139.41** | **61.70** |
| Additional live handoff and search startup/build | 41.76 | 1.71 |
| **Canonical and search ready, elapsed** | **181.17** | **63.41** |

Metadata was observed ready at 10.22 / 1.02 seconds from process launch. The daemon's internal milestone clock reported metadata at 9.58 / 1.00 seconds and canonical readiness at 138.77 / 61.69 seconds. This clock starts after executable startup; it is not a conflicting second measurement of the same origin. Total process lifetimes, including graceful shutdown, were 181.94 / 63.92 seconds.

### Timers inside those stages

Seconds. These are explanatory counters nested inside the phase table, not additional elapsed phases. Ledger worker timers overlap foreground streaming and may include nested flush time; do not sum this table to calculate startup duration. The canonical-application rows here use the `apply-prepared` stage delta, excluding the planning store's writes during capture.

| Work | Claude Code | Codex |
|---|---:|---:|
| Source reads | 22.30 | 17.51 |
| Parse and normalize | 20.84 | 14.55 |
| Apply to the RAM planning store during capture | 11.80 | 0.61 |
| Ledger append/foreground wait, cold total | 3.61 | 7.14 |
| Ledger framing | 7.89 | 7.44 |
| Ledger buffered writes | 12.07 | 20.41 |
| Ledger flushes | 9.86 | 2.96 |
| Canonical application within prepared-runtime application | 23.86 | 5.35 |
| RAM tool reconciliation preparation | 11.97 | 5.53 |
| Release committed reconciliation trees and temporary plan | 3.73 | 1.26 |
| Release original source batches | 1.42 | 0.45 |
| Asset materialization | 0.66 | 0.18 |
| All cold SQLite commits | 0.47 | 0.04 |
| **Token and USD rollup, combined** | **1.25** | **0.48** |
| Index creation and ANALYZE | 1.88 | 0.57 |
| SQLite quick/FK validation | 4.56 | 1.46 |

All profiler counters, including smaller statement timers and zero-valued phases, are retained in `summary.json` and the per-agent `result.json` files in the evidence directory below. The USD figure is the whole token/cost aggregation pass, not isolated calculator time.

### Search and handoff observations

These durations are approximate boundaries observed through status polling, not native search-worker timers. The first observed indexing status already had 3,072 / 3,840 documents processed, so the preceding interval includes some initial indexing work. A phase shorter than a polling interval can be missed.

| Observed interval | Claude Code | Codex |
|---|---:|---:|
| Canonical publication to first observed indexing | 21.44 | 0.36 |
| Indexing until next observed stage | 15.71 | 0.78 |
| Optimizing | 2.08 | Not separately observed |
| Validating | 2.28 | 0.31 |
| Publishing to Ready | 0.26 | 0.26 |

Codex's optimizing time is included in its observed indexing interval. Claude's 21.44-second initial interval is an attribution gap across live handoff/catch-up and search startup; this run does not isolate its individual function costs. It should not be described as pure FTS indexing time. The indexed document counts were 128,982 / 23,925.

### Scope, load and interpretation

| Quantity | Claude Code | Codex |
|---|---:|---:|
| Cold source payload bytes | 6,092,723,157 | 7,069,727,078 |
| Cold records framed | 1,855,669 | 1,047,866 |
| Discovered sessions | 6,379 | 322 |
| Canonical messages at shutdown | 419,890 | 178,591 |
| Peak child RSS, GiB | 7.23 | 5.03 |
| System 1-minute load-average range | 116.2–229.0 | 86.4–146.8 |
| System compressor occupancy, GiB | 14.05–22.96 | 15.47–18.82 |

Both cold streams had zero hard parsing errors, incomplete sources, or checkpoint conflicts. Claude's final session table includes 550 Pending metadata-only identities with no ingest cursor; these are not unfinished source obligations. 538 have child sessions. Codex's 322 sessions are all Indexed. Final row counts include the small live catch-up before search became ready; cold payload/record figures come from the earlier `serve-cold` snapshot.

Streaming and prepared-runtime application dominate both canonical builds. Reconciliation cleanup was measurable here but did not reproduce the earlier combined run's large destruction stall: trees plus source batches took 5.15 seconds for Claude and 1.72 seconds for Codex. This is a single-run stage diagnosis under varying host load, not a frozen before/after comparison or proof of a speedup from the pricing cleanup. The single-agent times do not predict the combined run's peak memory or concurrent scheduling behavior.

Evidence is retained locally in `/private/tmp/rosetta-agent-stage-profile-20260925/`: the measurement harness, code patch, campaign metadata, timestamped logs, stage profiles, status/resource samples, completion checks, and combined `summary.json`. The temporary benchmark databases, ledgers, assets and search indexes were removed after both runs completed, reclaiming 4.79 GiB; `cleanup.json` records the exact directories and sizes. The original app database and native histories were not modified by the harness. Cold replay/projection tests passed (26 tests), and the affected-crate Clippy and formatting checks passed with warnings denied for Clippy.

## Further optimization of the largest costs

The next pass found avoidable work at three boundaries:

1. **Live catch-up:** the old poll admitted every matching layout stream into its stat/membership work, including streams with no framing driver. It then linearly searched the entire admitted roster for each physical file and again when mapping an unknown path. The current native Claude inventory had 31,937 inventory-only files and 9,081 admitted sources. The new poll considers only streams with drivers and builds one temporary path index. Discovery still reports inventory; transcript admission, cursor-relative growth checks, normalizer expiry, snapshot membership and audits remain intact.
2. **Capture and reconciliation ownership:** JSONL batches now move into the planner. Closed source application also consumes its batches, moving deferred tool/usage/edit facts into provisional deltas and releasing immediate payloads incrementally. The runtime transaction commits before those deltas enter the shared reconciliation buffer. Opener selection borrows retained candidates and copies only the selected row into the retryable plan. This removes deep copies and the second runtime-sized source tree that previously survived the entire fold. Fingerprints, checkpoint CAS, rollback and closed-runtime publication remain enforced.
3. **Search startup:** `search_document_count` previously scanned the payload-bearing `message` table before reporting Indexing. A diagnostic read of the retained Claude projection counted 107,664 documents in **7.10 s**, with `EXPLAIN QUERY PLAN` reporting `SCAN message`. The deferred partial `message_searchable(id, type)` index contains only user/agent rows. It covers that count and supports the subsequent id-ordered search scan. It does not increase the indexed document set.

### Frozen comparison and host-load limits

Campaign `cold-topsteps-20260925` reused the existing permission-read-only source snapshot at `cold-sources-20260826/source-home`, selecting Claude Code and Codex independently. This is an older snapshot, **not the live-history corpus in the preceding section**. All 57,175 files / 10,993,320,551 bytes across its two roots were SHA-256 inventoried; source manifest SHA-256 is `7428042f111a62a3b0214f92e0345eb8b2e786661201a3ae3bf5c5d4e7b69e1c`. Actual cold payload was 3,050,162,408 bytes for Claude and 4,759,794,555 for Codex. The unchanged `product-diagnostic-v3` query-suite SHA-256 is `12e38e5ecc9a0e3d89bdd15b4c9fcc429defd07325f685cb108e91a97465aee7`.

Baseline binary: `fc4629beba50dabebf4752e59d523667e2a99496aed02b72c4af9bf0cd7cf238`. Final candidate: `57bbfef1cd1666c7837b2b76e1a52cf39f871624cea4aec4caa8db821541b8bf`. Both are release builds on the same M1 Max/64 GiB Mac, using serial single-agent fresh `serve --json --profile` runs with no renderer and fresh SamsungRed outputs. Compilation is excluded and filesystem caches were not evicted. Each daemon exited successfully after both canonical and search readiness were observed.

Seconds; native stage timers except the observed readiness/search boundaries:

| Stage | Claude before | Claude after | Codex before | Codex after |
|---|---:|---:|---:|---:|
| Discovery | 0.79 | 1.39 | 0.22 | 0.17 |
| Metadata | 9.01 | 13.29 | 0.86 | 0.59 |
| Capture and normalization | 59.85 | 50.03 | 27.74 | 37.34 |
| Closed-plan preparation | 1.63 | 2.72 | 0.56 | 0.69 |
| Application and reconciliation | 22.03 | 26.83 | 8.55 | 12.54 |
| Finalization | 3.86 | 8.31 | 1.70 | 3.35 |
| Durable publication | 5.16 | 4.02 | 0.81 | 1.25 |
| RAM release/reopen | 0.10 | 0.19 | 0.03 | 0.04 |
| **Canonical ready from process launch** | **104.00** | **109.16** | **40.61** | **56.13** |
| Additional handoff and search build | 25.46 | 15.69 | 1.24 | 1.74 |
| **Canonical and search ready** | **129.46** | **124.85** | **41.85** | **57.87** |

Readiness totals also include executable startup, coordination and observation latency. Claude's canonical-to-first-observed-indexing gap was **13.70 → 1.57 s**. The final native catch-up timer was 1.436 s for Claude and 0.206 s for Codex; Claude's poll spent 0.601 s walking and 0.255 s reading/decoding checkpoints, and considered 8,194 physical sources. Codex's short indexing phase was missed by polling in both final comparison runs, so no equivalent indexing-start gap is claimed.

Peak child RSS was 7.03 → 4.82 GiB for Claude and 4.23 → 3.02 GiB for Codex. These are observations, **not trustworthy percentage reductions in allocated memory**: macOS compression and paging change resident counts. System one-minute load ranged 54–110 during the Claude baseline versus 280–364 during its candidate, and 71–97 versus 254–288 for Codex. Compressor occupancy ranged roughly 16–22 GiB across these runs.

An additional unchanged-binary Codex control on the same frozen sources reached full readiness in **182.48 s**, versus **41.85 s** initially (4.36×), with identical headline row counts. Its capture/apply phases rose from 27.74/8.55 to 58.30/38.36 s, and load ranged 224–424. Its peak RSS fell to 2.60 GiB despite being dramatically slower, illustrating why RSS alone is not a heap-footprint comparison under this pressure. An exploratory partial candidate also took 190.80 s for Claude while load reached 342; its raw results are retained. None of these runs authorizes a clean end-to-end percentage speedup claim or a prediction for all agents started together. They corroborate severe host contention and identify smaller, concrete sources of redundant work.

### Correctness and remaining work

The final store/pipeline/search test run passed **256 tests** (one pre-existing opt-in test ignored), and affected-crate/all-target Clippy passed with warnings denied. Tests exercise both planner entry points, failed checkpoint CAS, metadata boundary enforcement, deferred-payload rollback followed by a successful retry, live silent appends, normalizer expiry, and the count query's covering index. Full frozen projections match across **17 canonical tables** and all asset files. Normalization excludes process discovery/drift timestamps, projection epoch/search revision, and the source-audit rotation cursor; native timeline, tool, usage, pricing, checkpoint and ownership facts are compared.

Both frozen ranked-search comparisons passed: 10 product-grammar cases × 100 measured cycles × two backend lanes × four projections = **8,000 measured queries**, with identical hit counts and ordered-result digests. The unchanged Codex control also matches the same full logical/asset digest. All 57,175 source file hashes, sizes and read-only permissions were reverified, and no files were added or removed. Quiesced output databases were checkpointed and switched to DELETE journaling before the permission-read-only search checks so readers did not need to create WAL shared-memory files.

A final count-query probe on each candidate database compared the production predicate with and without its index in the same read-only connection. Claude's identical 107,664-document count took **5.419 s with `NOT INDEXED` versus 0.00937 s with the covering index**; Codex took **2.417 s versus 0.00337 s**. A subsequent warm indexed count took 0.00923/0.000315 s. Both actual full-document visitor query plans use `message_searchable` in id order. These isolate redundant count/scan work; they are not end-to-end startup percentages.

Capture remains the largest phase. Further work should profile parsing/framing allocations and the planner's repeated control/checkpoint writes, then evaluate bounded per-file capture parallelism against a byte budget. Reconciliation still serializes deferred operations for retry fingerprints and retains a retryable final plan; eliminating those requires preserving the existing conflict and rollback contracts. USD/token rollup remains small compared with capture and application. Dropping raw history or publishing partially reconciled rows is not part of these changes.

Detailed evidence and reusable local harnesses are under `/private/tmp/rosetta-cold-topsteps-20260925/`, including source/query checksums, exact code patches and binary hashes, stage/status/resource traces, integrity/equality reports and test logs.

After verification, the seven temporary projection/ledger/asset/search output trees were removed, reclaiming **9.21 GiB**. The existing frozen source snapshot, three benchmark binaries, code patches, harnesses and structure-free evidence reports were retained. The ordinary app projection and native agent histories were not changed by this campaign.

## Earlier full-history remeasurement

Campaign `rosetta-cold-remeasure-20260925` reran the **full current native Claude Code and Codex histories**, independently and serially, with the final optimized release. Both reached canonical and search readiness successfully: **214.06 s (3m 34s) for Claude Code** and **114.27 s (1m 54s) for Codex**.

Binary SHA-256: `57bbfef1cd1666c7837b2b76e1a52cf39f871624cea4aec4caa8db821541b8bf`. The production code patch SHA-256 remained `bc3144849ec4ce244edfec05a46e758bcf4fcab15cf80bb422ab12e85882156c` throughout. No production code changes or rebuild were needed for this measurement; this is the same final code validated in the preceding section.

Each process used a new database, ledger, asset store and search index on SamsungRed, without a renderer. Native source histories remained live. OS filesystem caches were not evicted, so these measure fresh application state rather than a reboot with empty OS caches. The M1 Max / 10-core / 64 GiB host was heavily loaded. Each process received SIGINT only after HTTP status confirmed both readiness conditions and then exited with code 0.

### All main phases

Seconds. Native stage durations partition the canonical build, with executable startup, coordination and observation latency in the residual row. The catch-up duration is also a native timer. Search plus its small remaining handoff is the observed residual through full readiness. Totals start at process launch.

| Stage | Claude Code | Codex |
|---|---:|---:|
| Discovery | 11.50 | 0.52 |
| Metadata ingestion | 17.56 | 1.16 |
| Source reading, parsing, normalization and ledger capture | 82.51 | 92.34 |
| Closed source-plan preparation | 4.05 | 1.50 |
| Source application and reconciliation | 35.42 | 10.33 |
| Finalization: token/USD rollups, indexes, validation | 12.58 | 2.07 |
| Durable publication | 5.30 | 3.88 |
| Release RAM and reopen published database | 0.39 | 0.03 |
| Other startup/coordination and observation | 1.69 | 0.29 |
| **Canonical ready, elapsed** | 171.00 | 112.11 |
| Live catch-up | 16.42 | 0.85 |
| Search startup/build and remaining handoff | 26.63 | 1.30 |
| **Canonical and search ready, elapsed** | 214.06 | 114.27 |

Metadata was observed ready at **30.27 / 1.90 s**. The daemon's internal clock reported metadata at 29.54 / 1.88 s and canonical readiness at 170.27 / 112.09 s. Process lifetimes including shutdown were 215.16 / 114.71 s.

### Timers within the phases

These explain the phase totals above and must not be added to them. Ledger worker timers overlap foreground work and can include nested flush time. Canonical application uses the `apply-prepared` delta, excluding the planning store during capture. Original source-batch release is now incremental within owned application, so there is no separate source-tree destruction phase.

| Work | Claude Code, s | Codex, s |
|---|---:|---:|
| Source reads | 30.59 | 39.10 |
| Parse and normalize | 27.58 | 36.90 |
| Apply to RAM planning store during capture | 13.89 | 0.48 |
| Ledger append/foreground wait, whole cold build | 6.78 | 15.27 |
| Ledger worker framing | 10.57 | 15.63 |
| Ledger worker buffered writes | 21.55 | 47.21 |
| Ledger worker flushes | 11.10 | 5.61 |
| Canonical application inside prepared-runtime application | 20.33 | 3.68 |
| RAM tool reconciliation preparation | 6.27 | 3.38 |
| Release reconciliation trees and temporary plan | 1.73 | 0.57 |
| Asset materialization | 1.82 | 0.15 |
| All cold SQLite commits | 0.82 | 0.04 |
| **Token and USD rollup, combined** | 0.93 | 0.33 |
| Index creation and ANALYZE | 1.83 | 0.53 |
| SQLite quick/FK validation | 9.76 | 1.18 |
| Asset sync | 0.04 | 0.03 |
| Live source poll | 16.10 | 0.14 |
| Filesystem walk within live poll | 3.34 | 0.12 |
| Checkpoint reads/decoding within live poll | 9.30 | 0.01 |

Capture remains the largest elapsed phase for both agents. For Claude, reads, normalization and planner application account for 30.59, 27.58 and 13.89 s. For Codex, reads and normalization account for 39.10 and 36.90 s, with another 15.27 s in foreground ledger append/wait across the cold build. The 47.21 s of Codex ledger worker writes overlaps those foreground phases.

Claude's full native-history catch-up is still material: 16.42 s, including 9.30 s reading/decoding checkpoints and 3.34 s walking files. It considered 9,082 physical sources. Codex's poll considered 557 physical files and took 0.14 s; its full catch-up also replayed live changes. Claude/Codex catch-up consumed another 1,256,241 / 68,466 payload bytes and 138 / 41 records. The earlier 1.44 s Claude catch-up used the smaller frozen snapshot under different cache/load conditions.

### Search substages

These are approximate boundaries from HTTP polling, not native per-function timers. The harness slept 100 ms between requests after application completed, but requests, resource probes and scheduling add latency. All four stages were observed in both runs. The first Indexing sample had already processed 0 / 2,304 documents; the initial interval therefore includes some Codex indexing.

| Observed interval | Claude Code, s | Codex, s |
|---|---:|---:|
| Canonical readiness to first Indexing observation, including catch-up | 16.49 | 0.91 |
| Catch-up completion to first Indexing observation | 0.06 | 0.06 |
| Indexing until next observed stage/Ready | 17.78 | 0.43 |
| Optimizing until next observed stage/Ready | 1.82 | 0.11 |
| Validating until next observed stage/Ready | 6.65 | 0.59 |
| Publishing until next observed stage/Ready | 0.31 | 0.11 |

Search indexed **129,100 / 23,958 documents**, exactly matching each final projection's user-plus-agent message count. The final not-ready/ready polling brackets were 0.315 / 0.105 s; the largest post-canonical response gaps were 0.957 / 0.358 s. There were no HTTP failures before readiness. Codex had one expected connection refusal after SIGINT during shutdown.

### Scope, host pressure and completion checks

| Quantity | Claude Code | Codex |
|---|---:|---:|
| Cold source payload bytes | 6,110,276,455 | 7,076,914,157 |
| Cold records framed | 1,861,314 | 1,049,270 |
| Cold physical bytes read, including metadata | 6,533,820,137 | 7,332,357,507 |
| Discovered sessions | 6,380 | 322 |
| Canonical messages at shutdown | 420,547 | 178,747 |
| Ingest cursors, including virtual members | 9,082 | 1,108 |
| Inventory-only files, not transcript-ingested | 31,953 | 49 |
| Search documents | 129,100 | 23,958 |
| Peak child RSS, GiB | 6.35 | 3.81 |
| System 1-minute load-average range | 194.2–442.1 | 124.9–174.8 |
| System compressor occupancy, GiB | 20.11–26.18 | 19.92–24.07 |

Both streams reported zero hard errors, unknown records, oversized records, incomplete/missing sources, retries exhausted and checkpoint conflicts. Discovery was not truncated and had no unidentified or vanished files. Native canonical quick/FK validation and search integrity/quick checks completed before readiness. Session states were 5,830 Indexed plus 550 Pending for Claude, and 322 Indexed for Codex; the source-completion checks above cover all admitted source obligations.

The earlier full-native-history run reached full readiness in 181.17 / 63.41 s, compared with 214.06 / 114.27 s here. Payload growth was only 0.288% / 0.102%. Application/reconciliation is lower in this run (49.22 → 35.42 s and 16.05 → 10.33 s), while capture is higher (60.93 → 82.51 s and 40.20 → 92.34 s). These are observed deltas under different host pressure, not isolated code speedups or regressions. The unchanged-binary frozen control in the preceding section already demonstrated a 4.36× whole-startup swing; another single pair cannot resolve that confounder. RSS is also affected by compression and is not an allocated-heap comparison.

All raw profiler counters, including smaller/zero-valued timers, are retained in `summary.json` and the per-agent `result.json` files under `/private/tmp/rosetta-cold-remeasure-20260925/`. That directory also contains exact code/binary hashes, the measurement and summarization scripts, timestamped stage logs, status and resource samples, and `completion-checks.json`.

The two disposable benchmark output trees were removed after completion, reclaiming **4.80 GiB**; `cleanup.json` records their exact paths and sizes. Native source histories and the ordinary app database were not changed by the harness. These are separate agent runs; they do not measure the renderer or predict a concurrent all-agent cold start.

## Capture investigation and ledger optimization

The 82.51 / 92.34 s capture phase above is a serial read/normalize/planner loop per runtime, feeding one bounded asynchronous ledger worker. The worker overlaps that loop; its framing, compression and flush timers must not be added to foreground elapsed time. A full queue makes ledger work visible as foreground waiting.

### What consumes the time

- **Read is more than disk I/O.** `stream.rs` times the entire `Tailer::read` call: open/stat/seek, continuity and prefix checks, delimiter framing, payload allocation, and the resumable FNV content chain over every byte. The default batch admits 1,024 records, bounded by the 32 MiB hard limits, using 64 KiB reads and a 4 KiB prefix anchor. The latest full-history run physically read 6,177,762,046 / 7,290,045,196 bytes during capture, versus 6,110,276,455 / 7,076,914,157 payload bytes: only **1.10% / 3.01%** amplification. Metadata reads are separate. This is not repeated ingestion of the whole history for each batch.
- **Parse/normalize allocates a JSON tree first.** Both adapters' `parse_line` functions deserialize a `serde_json::Value`, select the record type, then convert known records into typed structs with `from_value`. Native stacks show JSON map/string allocation, unescaping and typed deserialization. Claude also parses declared but unprojected records to check their field vocabulary; these remain lossless ledger evidence. Eliminating the intermediate tree or narrowing that validation is a further candidate, but must preserve unknown-field drift, invalid-JSON detection and duplicate-key behavior.
- **Ledger framing repeated small work millions of times.** The owned append path still emitted one checksummed `TAG_RAW` frame per record, with repeated header/hash setup, path construction, generation bookkeeping and buffered writes. The reader already supported `TAG_RAW_BATCH`; the writer was not using it.
- **Private cold checkpoints flushed too often.** Physical sync was already deferred until publication, but each intermediate source batch still drained the compression stream. Those extra flushes did not make an unfinished, unsealed source eligible for recovery.

Five-second native stack samples taken after 100,000 records corroborate the read/framing, JSON allocation and ledger compression/checksum paths. These are diagnostic snapshots, not a whole-run CPU percentage. Inventory-only files were already excluded: 31,953 Claude and 49 Codex files in the latest full-history run were not transcript-ingested. Further improvements should reduce processing overhead while keeping the replay evidence.

**Host pressure is a major wall-time multiplier in the new comparison.** During the candidate's Claude capture, the nearest resource samples span 412.14 s but the daemon uses only 49.47 CPU-seconds across all its threads: an average of **0.12 CPU cores**, versus 37.38 CPU-seconds over 40.64 s (**0.92 cores**) in the baseline. This is not a direct I/O-wait measurement, but it shows that most elapsed time was not spent executing the daemon's instructions. Load averages exceeded 500 on a 10-core host. A process snapshot found 1,372 processes, including 69 Node processes and 149 Chrome renderers; `fseventsd` alone reported 109.7% CPU and 14% of physical memory. The daemon reported only 8.5% CPU at that instant. Unchanged Claude discovery grew 0.58 → 8.04 s, plan preparation 1.83 → 55.73 s and application 21.21 → 361.15 s. Scheduling, paging and I/O contention therefore confound the full comparison; these observations do not identify the historical cause of last week's regression by themselves.

### Retained changes

The owned append path now packs small records from one source/generation into the existing batch-frame format, with a **1 MiB body cap**. Large and single records keep the individual-frame path. Payload bytes, order, offsets, generation identity and limits are preserved; physical frame and segment boundaries can change. Packing adds at most 1 MiB per ledger worker, with the existing bounded queue unchanged.

The private asynchronous writer now coalesces intermediate checkpoints until metadata, a source seal or segment rotation. Ordinary durable checkpoints keep their behavior. The final joined flush still drains and syncs every current/rotated segment plus required directories before publication. A torn batch exposes none of that frame's records, and an unfinished raw-only generation remains ineligible for recovery.

Raw framing and ledger tests were split into focused modules. New tests cover both containers, small/large records, multiple sources/generations, rotation, torn/corrupt batches, metadata/seal boundaries, long unsealed sources and nondeferred checkpoint durability. The affected capture/pipeline/daemon suite passes **230 tests**, with the two existing opt-in host-notification/local-image tests ignored. Clippy passes for all targets of those crates with warnings denied.

### Isolated experiments

Release probes use the real tailer and ledger implementation on the largest frozen source files, alternating **before, after, after, before**. They time ledger calls and final flush, then decode the produced ledger and compare every payload/offset. The 108,950 Claude and 145,659 Codex records match exactly in every run. These probes isolate grouping; they use synchronous deferred checkpoints and therefore do not measure the additional async checkpoint coalescing.

| Ledger probe | Claude Code | Codex |
|---|---:|---:|
| Before, two runs, s | 0.994 / 1.032 | 2.611 / 3.315 |
| After, two runs, s | 0.911 / 0.861 | 2.356 / 2.295 |
| Mean ledger time, before → after | 1.013 → 0.886 | 2.963 → 2.326 |
| Mean reduction in this probe | 12.5% | 21.5% |
| Frames, before → after | 108,957 → 339 | 145,666 → 921 |
| Compressed bytes, before → after | 60,610,567 → 59,050,821 | 113,990,810 → 111,686,055 |

Host load was still 34–56 on the 10-core M1 Max, so these are observed small-probe results, not full-startup percentage claims. The probe's diagnostic payload digest is a 64-bit `DefaultHasher`, supplemented by the checksummed source manifest and full-build correctness checks below.

A separate delimiter-scanning experiment using `memchr` was **reverted**. Initial apparent gains were confounded by file-cache warming. Against the final warm baseline, Claude read time was 0.498 s versus 0.464/0.469 s, and Codex was 1.749 s versus 1.802/1.701 s. That does not establish a robust cross-corpus improvement. The retained change adds no dependency and leaves the tailer/checkpoint format unchanged.

### Full frozen before/after pair

Campaign `capture-opt-20260925` reused the permission-read-only `.claude`/`.codex` snapshot from `cold-sources-20260826`: **57,175 regular files / 10,993,320,551 bytes**, manifest SHA-256 `7428042f111a62a3b0214f92e0345eb8b2e786661201a3ae3bf5c5d4e7b69e1c`. The captured payloads are 3,050,162,408 / 4,759,794,555 bytes; this older snapshot is smaller than the latest live histories above. The checksummed `product-diagnostic-v3` query suite is `12e38e5ecc9a0e3d89bdd15b4c9fcc429defd07325f685cb108e91a97465aee7`.

Baseline binary SHA-256: `57bbfef1cd1666c7837b2b76e1a52cf39f871624cea4aec4caa8db821541b8bf`; candidate: `67f135d8a427a8826542de524aec67bca7b90028c7f278ab15f0e55f1588af5d`. The baseline/candidate build-time code patches hash to `bc3144849ec4ce244edfec05a46e758bcf4fcab15cf80bb422ab12e85882156c` / `0b2c476e3f391aea6597016e394640288ba0465e58ef2ab075d8afecacecd347`. The latter includes the newly split raw writer and tests. Subsequent Rust edits only clarify comments; the final source patch is retained separately.

Each fresh release `serve --json --profile --agent KIND --home SOURCE_HOME` ran alone through HTTP-confirmed canonical and search readiness, with a new database, ledger, assets and search index on SamsungRed. Each run took the same five-second native sample after at least 100,000 capture records. OS caches were not evicted. All four daemon processes exited with code 0 after readiness; the heavily loaded Claude candidate took another 41.86 s to shut down. No builds or bulk verification scans ran alongside these timed processes.

**This pair does not demonstrate an end-to-end speedup.** Host conditions changed sharply between the baseline and candidate. All durations below are observed seconds, with no adjustment for contention.

| Stage | Claude before | Claude after | Codex before | Codex after |
|---|---:|---:|---:|---:|
| Discovery | 0.58 | 8.04 | 0.20 | 9.21 |
| Metadata | 8.69 | 35.19 | 0.69 | 1.23 |
| Read, parse, normalize, capture ledger | 41.46 | 414.92 | 37.22 | 251.35 |
| Closed source-plan preparation | 1.83 | 55.73 | 0.64 | 1.22 |
| Canonical application and reconciliation | 21.21 | 361.15 | 9.78 | 15.92 |
| Finalization | 3.97 | 220.29 | 1.83 | 3.46 |
| Durable publication | 3.98 | 25.50 | 2.52 | 1.10 |
| RAM release and reopen | 0.13 | 27.60 | 0.03 | 0.11 |
| Other setup/coordination | 0.64 | 4.87 | 0.13 | 1.37 |
| **Canonical ready** | 82.49 | 1153.30 | 53.03 | 284.97 |
| Live catch-up | 1.71 | 83.60 | 0.15 | 0.21 |
| Search and remaining handoff | 8.82 | 346.48 | 1.00 | 1.29 |
| **Canonical and search ready** | 93.01 | 1583.37 | 54.19 | 286.47 |

Nested foreground timers explain the capture phase. The ledger foreground timer spans the whole cold build. Worker timers overlap foreground work and remain available in `summary.json`.

| Work, seconds | Claude before | Claude after | Codex before | Codex after |
|---|---:|---:|---:|---:|
| Read and frame | 20.30 | 116.59 | 20.75 | 105.73 |
| Parse and normalize | 12.27 | 123.87 | 12.83 | 123.26 |
| RAM planner application | 7.10 | 112.45 | 0.21 | 1.46 |
| Ledger foreground append/wait | 0.53 | 32.05 | 3.21 | 18.00 |
| Worker framing/packing | 3.99 | 25.38 | 6.06 | 32.02 |
| Worker buffered writes/compression | 3.99 | 46.18 | 13.80 | 114.43 |
| Worker flush | 5.77 | 35.84 | 2.62 | 4.67 |

The counts demonstrate less ledger work without less captured data:

| Ledger work | Claude before | Claude after | Codex before | Codex after |
|---|---:|---:|---:|---:|
| Frames | 1,221,418 | 24,130 | 885,677 | 6,754 |
| Buffer flushes | 11,572 | 8,206 | 1,890 | 783 |
| Segment syncs | 23 | 23 | 35 | 35 |
| Uncompressed frame bytes | 3,097,996,446 | 3,072,912,693 | 4,789,444,585 | 4,771,027,309 |
| Compressed disk bytes | 603,409,289 | 586,087,233 | 869,789,018 | 855,166,985 |

Frame counts fall **98.0% / 99.2%**, flush counts **29.1% / 58.6%**, and compressed bytes **2.9% / 1.7%**, with unchanged final segment-sync counts. The final barrier still covers every segment. These deterministic changes and the isolated alternating probes support the ledger optimization; they do not establish the full-startup percentage benefit.

| Host/process observation | Claude before | Claude after | Codex before | Codex after |
|---|---:|---:|---:|---:|
| 1-minute load-average range | 59.1–143.1 | 528.7–736.8 | 49.1–70.8 | 578.3–677.8 |
| System compressor occupancy, GiB | 17.55–21.57 | 20.85–33.10 | 19.17–21.40 | 22.83–27.56 |
| Peak child RSS, GiB | 4.91 | 1.69 | 3.08 | 2.97 |
| Child user + system CPU, s | 74.88 | 149.61 | 46.54 | 52.72 |

RSS falls when pages are compressed or paged out; it is not evidence of a smaller allocated heap. Child CPU includes the small resource probes and native sampler, while the earlier stage-specific CPU comparison uses only the daemon's `ps` counters. The system recorded 88.8 million page decompressions during the Claude candidate; that is a host-wide counter, not work attributable solely to Rosetta. Codex's late capture, application and search became much faster as resource availability changed during the same process, even while the trailing load average remained high.

### Losslessness checks

Full decoded-ledger comparison passes for **2,089,161 records / 7,809,956,963 payload bytes**, including ordered source identities, generations, offsets and all 8,944 completion markers. Metadata frame counts also match; process-timestamped metadata payloads are checked through the normalized canonical comparison. Both sides have zero torn segments. The ordered BLAKE3 digests are `7aa1088497dc449b7b2e6197265961f65ae02cd9d6e8ecbdc94f463415bf54c2` (Claude) and `699bd4ae85a004ac82dec07cf285e31cb65ee847791ee1cd655fa07c3c83e7c1` (Codex).

The post-run scan reverified every source file's SHA-256, byte count, read-only permission and membership in the selected runtime roots. The snapshot and query-suite hashes are unchanged. Source symlinks and unrelated runtime roots are outside this campaign's manifest.

All **17 canonical tables** match for each runtime after excluding only process bookkeeping (`agent_runtime.discovered_at`, drift observation times, projection epoch/search revision) and the physical ledger-position `source_audit` table. Every asset matches by name, size and SHA-256: **646 Claude / 99 Codex members**. All four projections pass SQLite quick checks with zero foreign-key violations. Every build reports zero missing/incomplete sources, exhausted retries, hard/unknown/oversized records and checkpoint conflicts; discovery is complete and untruncated.

The required search comparison passes: ten diagnostic cases, two backend lanes, five warmup cycles and 100 measured cycles on each of the four projections, for **8,000 measured queries** with identical hit counts and ranked-result digests. Both sides of each query pair use the same checksummed baseline ledger and query suite. Outputs were made read-only after a clean WAL checkpoint. Query latency reports are retained, but the host conditions do not support a latency improvement claim or production query weighting.

The capture/pipeline/daemon tests, all-target Clippy with warnings denied, release build, final formatting and whitespace checks pass. Exact commands, binary/source hashes, full stage/resource logs, native stacks, CPU observations, probes, source validation, raw/canonical/asset equality and search reports are retained in `/private/tmp/rosetta-capture-opt-20260925/`. The final code patch hash is `ced72e604b8e5d244ee3b1f66443a4ba62b36faca6543822297b0382d89cb8a5`.

After all comparisons passed, the four disposable output trees were removed, reclaiming **5.35 GiB**. `cleanup.json` records exact paths and byte counts. The frozen source snapshot, baseline/candidate executables, evidence and ordinary app data remain intact.

The next capture candidates are reducing intermediate JSON allocation and overlapping bounded read/parse work while preserving source ordering, mutation detection, drift reporting and closed-runtime reconciliation. The read-loop experiment here did not justify a retained change. A quieter repeat is needed to measure the ledger changes' end-to-end benefit; this investigation does not claim the historical cold-start regression is fully resolved.

## Latest full-history repeat after ledger optimization

Campaign `rosetta-cold-remeasure2-20260925` repeats separate full Claude Code and Codex cold starts with the ledger-optimized release, SHA-256 `67f135d8a427a8826542de524aec67bca7b90028c7f278ab15f0e55f1588af5d`. **Claude reached canonical and search readiness in 188.70 s (3m 9s); Codex in 58.41 s.** No production code changed or rebuild occurred during this measurement.

The processes ran sequentially against their full live native histories, each using fresh database, ledger, asset and search state on SamsungRed, without a renderer. OS filesystem caches were not evicted. These are fresh-application cold starts, not reboot/empty-OS-cache measurements. The preceding 26m 23s / 4m 46s results used the smaller frozen August snapshot; they are not comparable source sets.

| Stage, seconds | Claude Code | Codex |
|---|---:|---:|
| Discovery | 10.87 | 0.29 |
| Metadata ingestion | 11.33 | 0.44 |
| Read, parse, normalize and capture ledger | 63.91 | 40.70 |
| Closed source-plan preparation | 4.24 | 1.31 |
| Canonical application and reconciliation | 55.53 | 10.48 |
| Finalization: rollups, indexes and validation | 10.85 | 2.14 |
| Durable publication | 3.10 | 0.94 |
| Release RAM and reopen | 0.16 | 0.03 |
| Other startup/coordination and observation | 0.83 | 0.13 |
| **Canonical ready, cumulative** | **160.83** | **56.46** |
| Live catch-up | 6.20 | 0.92 |
| Search and remaining handoff | 21.67 | 1.03 |
| **Canonical and search ready, cumulative** | **188.70** | **58.41** |

Metadata was observed ready at 22.51 / 0.78 s. The native daemon clock reported canonical readiness at 160.77 / 56.45 s; full readiness was confirmed over HTTP with final polling brackets of 0.12 / 0.11 s. Native phase timers partition canonical build time; search and remaining handoff are an observed residual.

Nested timers explain the phases above and must not be added again. Ledger foreground time below spans the whole cold build; ledger worker timers overlap foreground work.

| Work, seconds | Claude Code | Codex |
|---|---:|---:|
| Read and frame | 24.89 | 19.04 |
| Parse and normalize | 21.13 | 14.38 |
| RAM planner application | 11.06 | 0.20 |
| Ledger foreground append/wait | 4.37 | 6.84 |
| Ledger worker framing/packing | 6.97 | 5.67 |
| Ledger worker buffered writes/compression | 16.45 | 21.35 |
| Ledger worker flushes | 7.07 | 1.67 |
| Canonical application within prepared-runtime application | 29.43 | 3.63 |
| Tool reconciliation preparation | 13.58 | 3.96 |
| Reconciliation cleanup | 3.65 | 0.51 |
| Token and USD rollup | 2.27 | 0.32 |
| Index creation and ANALYZE | 3.89 | 0.67 |
| SQLite quick/FK validation | 4.64 | 1.13 |

The cold streams processed 6,132,429,882 / 7,082,762,175 payload bytes and 1,867,539 / 1,050,587 records. Final projections contained 6,380 / 323 sessions and 421,302 / 179,197 messages. Inventory-only files skipped for transcript ingestion numbered 32,011 / 47. Both reported zero hard errors, unknown/oversized records, missing/incomplete sources, exhausted retries and checkpoint conflicts; discovery was untruncated with zero unidentified or vanished files. Both reached canonical and search readiness with no HTTP errors.

The host remained busy: 1-minute load ranges were 346.7–510.3 / 275.5–347.6, and compressor occupancy 22.72–28.05 / 21.68–26.24 GiB. Actual process CPU availability was much better than in the preceding frozen candidate run: nearest five-second samples show Claude capture using 61.07 CPU seconds over 66.74 wall seconds (0.92 cores average), versus 49.47 CPU seconds over 412.14 wall seconds previously. These samples are approximate and do not measure I/O wait. The high trailing load average alone does not describe the CPU time available to this process. This repeat establishes current observed timings, not an isolated code-speedup percentage.

Measurement bookkeeping: after Claude reached readiness, exited and emitted its final profile, Python's post-run read-only SQLite open failed. Timings, readiness and logs had already been saved. Counts were recovered with an immutable read after verifying no pending WAL; Claude's exact process exit code and final `getrusage` totals were not persisted and remain unavailable. The Codex harness retained those totals before counting and used the corrected post-shutdown read; Codex exited 0. Recovering Claude's counts briefly overlapped Codex's run. No timed ingestion/readiness code changed.

Full stage logs, timestamped status/resource observations, source-code hashes, recovered/raw result files, exact measurement scripts, CPU observations and completion checks are retained under `/private/tmp/rosetta-cold-remeasure2-20260925/`. The main phase table is derived from `summary.json`.

After validation, the two disposable output trees were removed, reclaiming **4.77 GiB**; exact paths and byte counts are retained in `cleanup.json`. Native histories and the ordinary app database were unchanged.

## Deferred ledger compression after readiness

Normal fresh and resumed RAM `serve` cold starts now write checksummed plain `RLG1` segments, with the existing durable flush before canonical publication. After publication and initial live catch-up, one daemon-owned maintenance thread waits for actual canonical/search readiness and compresses only an immutable snapshot of closed segments. Live appends and finite CLI ingest retain inline zstd. No source records, canonical fields, normalized operations, or recovery guarantees are removed.

The worker targets **10% of one CPU core**, using the macOS thread CPU clock, 5 ms work quanta, utility QoS, and interruptible pacing. Source validation, compression and decoded-copy validation have bounded read checkpoints. The original segment is replaced atomically only after the copy has been synced and verified byte-for-byte by frame checksums and a decoded-stream digest. Concurrent readers retain a valid descriptor. Shutdown cancels the current copy; warm starts rediscover remaining plain segments. Search readiness is rechecked during maintenance, so new search work pauses compression.

Utility QoS is intentional. A local native scheduling probe found strict background QoS made no progress for five seconds until thread join, whereas utility QoS ran immediately. Explicit CPU pacing supplies the low CPU target while allowing progress. Other platforms conservatively pace elapsed active time. This is an average target with bounded work bursts, not an instantaneous process-wide CPU limit.

Campaign `deferred-compression-20260925` compares the prior ledger-optimized release (`67f135d8a427a8826542de524aec67bca7b90028c7f278ab15f0e55f1588af5d`) with the new release (`1d42a432a779c8fcd42c71c7cacd35bbe1f3b440523b08b0df78bf0e3d60a256`). It uses the same permission-read-only August source snapshot and checksummed query suite on the Apple M1 Max, ten cores, 64 GiB. Each release/agent pair starts with fresh database, ledger, assets and search state on SamsungRed, without a renderer. Runs are sequential; OS filesystem caches are not evicted. This older frozen snapshot differs from the full current native histories in the preceding 3m 9s / 58.41s repeat.

| Stage, seconds | Claude before | Claude deferred | Codex before | Codex deferred |
|---|---:|---:|---:|---:|
| Discovery | 2.99 | 1.75 | 0.48 | 0.86 |
| Metadata ingestion | 16.96 | 10.00 | 1.51 | 2.31 |
| Read, parse, normalize and capture ledger | 112.06 | 58.57 | 49.36 | 116.17 |
| Closed source-plan preparation | 1.95 | 2.32 | 0.85 | 4.06 |
| Canonical application and reconciliation | 26.85 | 24.86 | 12.99 | 33.81 |
| Finalization: rollups, indexes and validation | 4.20 | 5.21 | 3.10 | 6.11 |
| Durable publication | 6.52 | 2.76 | 1.59 | 2.13 |
| Release RAM and reopen | 0.58 | 0.14 | 0.11 | 0.08 |
| Other setup, coordination and observation | 1.79 | 1.35 | 0.13 | 0.32 |
| **Canonical ready, cumulative** | 173.91 | 106.95 | 70.12 | 165.87 |
| Live catch-up | 6.06 | 1.16 | 0.23 | 0.37 |
| Search and remaining handoff | 32.38 | 5.68 | 2.77 | 2.95 |
| **Canonical and search ready, cumulative** | 212.34 | 113.79 | 73.11 | 169.19 |

The table separates startup readiness from later maintenance. Native stage timers partition canonical build time; search/handoff is an observed residual. Canonical totals use the observer clock. The daemon clock reported 173.88 / 106.23 s for Claude and 70.11 / 165.83 s for Codex. HTTP readiness polling brackets were at most 0.17 s; all four processes exited 0 with canonical/search readiness confirmed and zero HTTP errors.

**This pair does not establish a net cold-start speedup.** Claude improved while Codex slowed under changing host pressure. Nearest process-CPU samples during capture show 0.38 → 0.53 cores for Claude and 0.68 → 0.23 cores for Codex. Codex read and normalization wall times rose from 26.39 / 18.33 s to 65.72 / 47.02 s while the foreground ledger timer fell. Increased plain-write volume is also a tradeoff; these observations cannot isolate all causes of the elapsed-time changes.

| Stage, seconds | Claude before | Claude deferred | Codex before | Codex deferred |
|---|---:|---:|---:|---:|
| Ledger foreground append/wait and final drain | 9.70 | 1.01 | 3.77 | 0.98 |
| Ledger worker framing/packing | 8.90 | 3.57 | 5.62 | 11.35 |
| Ledger worker buffered writes | 16.00 | 1.49 | 18.69 | 9.22 |
| Ledger worker flushes | 12.97 | 1.75 | 1.28 | 1.97 |

These nested timers overlap the outer stages and each other. The plain writer performs no cold zstd compression. The foreground timer includes append/wait and the final drain; worker write/flush timers are wall time, not pure compression CPU, and must not be added to the cold total.

| Post-readiness maintenance observation | Claude | Codex |
|---|---:|---:|
| Started after first readiness observation | 0.92 s | 1.25 s |
| Wall time before deliberate interruption | 288.78 s | 89.11 s |
| Actual compression-thread CPU time | 9.97 s | 3.66 s |
| Average CPU, percent of one core | 3.45% | 4.10% |
| Closed segments successfully replaced | 9 | 4 |
| Bytes before → after, decimal GB | 1.217 → 0.254 | 0.549 → 0.078 |

The full-data maintenance observations were deliberately interrupted to exercise recovery instead of waiting for every throttled segment to finish. Remaining plain segments are valid, durable evidence. Complete compression and cancellation at every callback boundary pass the fixture tests. Both organic runs left no partial copy, torn segment, compression error or priority-setting error. Warm restarts subsequently replaced additional previously plain segments and shut down cleanly; all 17 canonical tables/assets and every ordered raw record/completion marker remained unchanged. A warm metadata admission adds its normal timestamped metadata frame, so that frame count is recorded separately from raw-record equality.

The frozen cold ledgers require **3.07 / 4.77 GB** before compression, versus **0.59 / 0.86 GB** with inline compression: approximately **6.40 GB of additional temporary disk space** across these two histories, plus one segment's temporary compressed copy. Maintenance also adds later verification/read/write passes. For the larger full live histories in the preceding repeat, the measured framed-versus-compressed byte counts imply roughly 10.1 GB of temporary additional ledger space. The eventual compression ratio can differ because the maintenance pass has different flush boundaries.

Validation: all **2,089,161 raw records**, **8,944 completion markers**, ordered source identities/generations/offsets, metadata frame counts, and both raw digests match in the cold before/after pair, with zero torn segments. All **17 canonical tables** and **646 / 99 assets** match, excluding only documented process timestamps, epoch/search revision and the physical-position `source_audit` table. All projections pass SQLite quick checks and foreign-key checks. The same ten-case, two-lane query suite, with five warmup and 100 measured cycles per projection, produces **8,000 measured queries with identical hit counts and ranked-result digests**. Search equality runs after the additional warm-restart preservation checks; query latency is not used to claim a performance improvement.

The source scan reverified all 57,175 files and 10,993,320,551 bytes, hashes, read-only permissions and scoped membership. Manifest SHA-256: `7428042f111a62a3b0214f92e0345eb8b2e786661201a3ae3bf5c5d4e7b69e1c`. Query-suite SHA-256: `12e38e5ecc9a0e3d89bdd15b4c9fcc429defd07325f685cb108e91a97465aee7`. Capture/pipeline/daemon tests passed (**237 passed, 2 ignored**), as did all-target Clippy with warnings denied, the release build, formatting and whitespace checks. All 28 candidate code files remained unchanged since the build and measurements. Candidate code patch SHA-256: `9d152b28feba8b33894d04cb2829e7d93c7469f3b3e7a467bd314739346c596e`.

Evidence, exact scripts, the original and adjusted post-readiness observation harnesses, scheduling probe, native stage logs, actual worker CPU events, interruption/restart results, raw/canonical/asset equality, source verification and search reports are retained in `/private/tmp/rosetta-deferred-compression-20260925/`. Only the post-readiness stopping policy changed during measurement; no cold timer or production code changed. Process-lifetime/resource totals include the deliberately observed background interval and must not be presented as cold-start totals.

After validation, the four disposable output trees were removed, reclaiming **9.74 GiB**. Exact paths and byte counts are retained in `cleanup.json`. Frozen sources, baseline/candidate executables, evidence and ordinary app data remain intact.

## Full-history remeasurement after deferred compression

Campaign `rosetta-cold-remeasure3-20260925-1651` reran full current native Claude Code and Codex histories, sequentially, with fresh database, ledger, assets and search state on SamsungRed and no renderer. It used the already-validated deferred-compression release, SHA-256 `1d42a432a779c8fcd42c71c7cacd35bbe1f3b440523b08b0df78bf0e3d60a256`. No production code changed and no rebuild occurred. OS filesystem caches were not evicted. **Claude reached canonical and search readiness in 622.96 s (10m 23s); Codex in 73.57 s (1m 14s).**

| Stage, seconds | Claude Code | Codex |
|---|---:|---:|
| Discovery | 18.78 | 0.72 |
| Metadata ingestion | 27.22 | 1.14 |
| Read, parse, normalize and capture ledger | 156.85 | 48.58 |
| Closed source-plan preparation | 5.18 | 1.10 |
| Canonical application and reconciliation | 322.20 | 14.14 |
| Finalization: rollups, indexes and validation | 37.33 | 2.69 |
| Durable publication | 4.44 | 1.48 |
| Release RAM and reopen | 0.43 | 0.04 |
| Other startup, coordination and observation | 3.94 | 0.10 |
| **Canonical ready, cumulative** | 576.35 | 69.99 |
| Live catch-up | 12.65 | 1.64 |
| Search and remaining handoff | 33.96 | 1.94 |
| **Canonical and search ready, cumulative** | 622.96 | 73.57 |

Metadata was observed ready at 47.85 / 1.93 s; the daemon clock reported 47.79 / 1.92 s. Native canonical milestones were 576.29 / 69.98 s. The cumulative table uses the observer clock, with final search-readiness polling brackets of 0.116 / 0.113 s. Both daemons exited 0. All status requests through readiness succeeded; each harness recorded one connection-refused response after it deliberately sent SIGINT and the listener closed.

| Stage, seconds | Claude Code | Codex |
|---|---:|---:|
| Read and frame | 40.23 | 26.56 |
| Parse and normalize | 38.26 | 19.65 |
| RAM planner application | 18.73 | 0.26 |
| Ledger foreground append/wait and final drain | 52.30 | 1.73 |
| Ledger worker framing/packing | 47.41 | 8.74 |
| Ledger worker buffered writes | 13.29 | 8.92 |
| Ledger worker flushes | 8.06 | 1.28 |
| Canonical apply within prepared-runtime application | 194.33 | 3.73 |
| Tool reconciliation preparation | 74.82 | 7.16 |
| Reconciliation cleanup | 6.23 | 0.80 |
| Token and USD rollup | 7.50 | 0.36 |
| Index creation and ANALYZE | 15.29 | 0.60 |
| SQLite quick/FK validation | 14.39 | 1.71 |

Nested timers explain the outer stages and overlap; they must not be added again to the cumulative totals. Ledger foreground spans the whole cold build, including enqueue/wait and final drain. The capture-only foreground ledger timer was 52.19 / 1.71 s. Cold ledger disk bytes exactly match framed bytes plus plain-container headers, and neither daemon emitted a background compression start event before shutdown. These ledger timers therefore cover framing, queueing, writes, flushes and durability work. Deferred maintenance is outside the readiness measurement.

**Claude's largest stage was canonical application/reconciliation: 322.20 s**, followed by capture at 156.85 s. Codex's largest stage was capture at 48.58 s. These are current observations under substantial host pressure. Claude's 1-minute load range was 276.3–568.1 and system compressor occupancy 21.46–33.71 GiB; Codex's ranges were 365.0–454.8 and 21.28–23.97 GiB. Approximate process-CPU samples around Claude application show 47.41 CPU seconds over 323.28 wall seconds (0.15 cores average); its capture averaged 0.35 cores. Codex capture and application each averaged about 0.67 cores. These samples do not distinguish descheduling from I/O or page-fault waits. This live-history repeat does not isolate a code-speedup or regression percentage.

The cold streams processed **6,173,962,237 / 7,111,222,865 payload bytes** and **1,883,536 / 1,056,425 records**. Final projections held **6,389 / 326 sessions** and **423,732 / 179,782 messages**. Both reported zero missing/incomplete sources, exhausted retries, hard/unknown/oversized records and checkpoint conflicts; discovery was untruncated with zero unidentified or vanished files. Inventory-only files skipped for transcript ingestion numbered 32,077 / 51. Canonical/search readiness and the cold path's native validation completed for both agents. Live sources may grow, so these inputs differ from the earlier frozen August comparison.

Exact scripts, binary/source hashes, complete stage profiles, readiness/resource logs, source-completeness checks and approximate per-stage CPU observations are retained in `/private/tmp/rosetta-cold-remeasure3-20260925-1651/`. The two disposable output trees were removed after validation, reclaiming **14.24 GiB**. `cleanup.json` records the paths and byte counts. Native histories, the ordinary app database, the executable and measurement evidence remain intact.

## Apply/reconciliation optimization — 2026-09-25

The preceding 622.96 s Claude full-history run spent 322.20 s in prepared-runtime application. Its nested timers were 194.33 s canonical application, 74.82 s reconciliation preparation, and 6.23 s cleanup. These are wall timers; they are not all SQLite execution. Nearest five-second process samples showed roughly 47.41 CPU seconds over 323.28 wall seconds around application. Load was 276–568 and system compressed memory occupied 21.46–33.71 GiB. This shows little CPU progress relative to elapsed time, but does not separate descheduling, memory faults, and I/O waits. Deferred ledger compression had not started.

Code inspection identified unnecessary work within that pressure-sensitive stage:

- Every activated source scanned all previously buffered sources to evict older generations. A fresh 9,100-source runtime therefore visited 41,400,450 unrelated entries. The new range lookup visits only generations of the affected cursor.
- Cold source rows repeatedly attempted the same session timestamp update. Each batch now accumulates the maximum per session and writes it before transaction commit; metadata batches retain their ordered semantics.
- Reconciliation copied update patches and kept both original and resolved tool payloads in the final plan. It now borrows patches during preparation and folds into one owned final row. Source buffers and fingerprints still survive failed commits.
- Canonical writes cloned complete messages and copied text/tool fields merely to encode them. Unchanged fields are now borrowed. Ordinary tool writes still enforce command compaction and watched live mutations still own their payloads.
- Tool result timestamps caused one or two extra row/index updates after each tool insert. The cold plan now includes those bounds in the initial write; updates to previously stored tools keep the existing path.

No raw history, normalization fields, publication barriers, or canonical schema were removed.

### Frozen comparison

Campaign `apply-opt-20260925` uses the same permission-read-only `cold-sources-20260826` snapshot: 57,175 files / 10,993,320,551 bytes. Manifest SHA-256 `7428042f111a62a3b0214f92e0345eb8b2e786661201a3ae3bf5c5d4e7b69e1c`; query-suite SHA-256 `12e38e5ecc9a0e3d89bdd15b4c9fcc429defd07325f685cb108e91a97465aee7`. Machine: Apple M1 Max / 10 cores / 64 GiB / macOS. Configuration: serial per-agent release `serve`, fresh database/ledger/assets/search, no renderer, stop at canonical plus search readiness. OS caches were not evicted; no stack sampling or concurrent builds were used during timing.

Baseline binary SHA-256 `1d42a432a779c8fcd42c71c7cacd35bbe1f3b440523b08b0df78bf0e3d60a256`. Candidate SHA-256 `811bd845bf1b9d4b97eb33a89b92d36739bba45bda5ce5f69a6fc62958066fef`. Source-hash manifests and dirty-patch snapshots, exact commands, raw events, resources, all equality reports, and reusable harnesses are in `/private/tmp/rosetta-apply-opt-20260925`. The candidate binary remains at `/Volumes/SamsungRed/rosetta-agent-tests/runs/apply-opt-20260925/candidate-rosettad`.

| Stage (seconds) | Claude before | Claude after | Codex before | Codex after |
|---|---:|---:|---:|---:|
| Discovery | 0.63 | 0.61 | 0.18 | 0.16 |
| Metadata | 6.53 | 7.17 | 1.15 | 0.68 |
| Read / parse / normalize / capture | 47.15 | 48.35 | 61.02 | 45.74 |
| Closed plan preparation | 2.14 | 2.01 | 0.87 | 0.84 |
| Apply / reconcile | 24.75 | 26.88 | 8.83 | 8.41 |
| Finalization | 4.77 | 5.48 | 2.25 | 2.12 |
| Durable publication | 3.30 | 2.53 | 1.31 | 2.36 |
| RAM release / reopen | 0.14 | 0.13 | 0.04 | 0.03 |
| Other setup / coordination | 0.54 | 1.33 | 0.14 | 0.10 |
| Canonical ready (observed) | 89.96 | 94.49 | 75.79 | 60.45 |
| Live catch-up | 3.32 | 1.20 | 0.19 | 0.16 |
| Search / remaining handoff | 43.92 | 9.62 | 1.63 | 1.16 |
| Full ready (observed) | 137.20 | 105.30 | 77.62 | 61.77 |

Session-touch SQL attempts fell from **397,583 to 10,574 (97.34%)** for Claude and **48,849 to 568 (98.84%)** for Codex. The new activity fixture also verifies that a cold tool with several result timestamps is inserted once with its final bounds, survives an injected failed flush, and requires no follow-up message update.

The lower full-ready totals are observations, not an isolated end-to-end speedup claim. Claude application itself was 24.75 → 26.88 s; Codex was 8.83 → 8.41 s. Claude baseline load ranged 55.5–251.6, candidate 247.3–296.3; Codex baseline 265.9–300.4, candidate 292.9–321.1. The unchanged capture/search phases also varied substantially. Whole-run child CPU (including the small resource probes) was 72.13 → 65.37 s for Claude and 35.60 → 33.81 s for Codex. Peak child RSS was 4.96 → 4.65 GiB and 3.04 → 2.89 GiB respectively; RSS is not a full footprint measurement under system memory compression.

Validation passed: 136 store tests and 104 pipeline tests (one opt-in test ignored); affected store/pipeline/daemon all-target Clippy with warnings denied; release build and formatting checks. The new tests cover timestamp maxima, a failed reconciliation flush followed by retry, exact activity bounds, and a single initial tool insert. Existing replacement, duplicate-evidence, cross-source, usage-ownership, rollback, assets, and watched-effect tests also pass.

Frozen before/after logical digests match all 17 compared canonical tables and every content-addressed asset. Process timestamps/epochs and physical source-audit positions are excluded exactly as in the previous campaigns. SQLite quick-check is clean and foreign-key violations are zero. Ordered raw-record identities, generations, offsets, payloads, and seals match for both agents. All 8,000 measured search executions preserve hit counts and ranked-result digests. The same 57,175 read-only source files and query-suite checksum were verified before and after. No benchmark output was removed before these checks completed.

### Current full native histories after optimization

These are fresh application-state cold starts against the current native home directories, run serially with the same candidate binary and no renderer. They include discovery through observed canonical and detached-search readiness. Filesystem caches were not evicted, native histories remain mutable, and the source set is larger than the frozen comparison; these measurements are not a controlled comparison with the earlier 622.96/73.57 s live-history runs.

| Stage (seconds) | Claude | Codex |
|---|---:|---:|
| Discovery | 4.38 | 0.86 |
| Metadata | 9.71 | 0.91 |
| Read / parse / normalize / capture | 62.31 | 86.48 |
| Closed plan preparation | 3.03 | 1.29 |
| Apply / reconcile | 37.98 | 9.49 |
| Finalization | 6.39 | 2.65 |
| Durable publication | 8.32 | 3.29 |
| RAM release / reopen | 0.25 | 0.27 |
| Other setup / coordination | 1.04 | 0.27 |
| Canonical ready (observed) | 133.41 | 105.49 |
| Live catch-up | 6.73 | 1.27 |
| Search / remaining handoff | 15.71 | 1.88 |
| Full ready (observed) | 155.85 | 108.65 |

Nested application timers below overlap the phase totals and must not be added to them.

| Work (seconds) | Claude | Codex |
|---|---:|---:|
| Canonical apply | 20.52 | 2.45 |
| Reconciliation preparation | 7.72 | 3.90 |
| Reconciliation cleanup | 3.41 | 0.49 |

The remaining largest phase is read/parse/normalize/capture. Its nested timers are:

| Work (seconds) | Claude | Codex |
|---|---:|---:|
| Source read / framing | 23.05 | 37.06 |
| Parse / normalize | 23.59 | 33.92 |
| RAM planner apply | 10.89 | 0.37 |
| Foreground ledger wait/work | 2.05 | 14.14 |
| Ledger worker buffered writes | 4.74 | 31.35 |

These timers overlap, including worker and foreground work. Codex's whole-run child CPU was 48.08 s, essentially the preceding run's 48.40 s, despite full readiness increasing from 73.57 to 108.65 s. The extra elapsed time is concentrated in capture (48.58 → 86.48 s), including increased ledger backpressure and buffered-write wall time. This indicates waiting/scheduling effects rather than a comparable increase in CPU work; the timers cannot separate physical I/O from descheduling. Ledger compression did not run before readiness.

- claude-code: 6,178,307,564 payload bytes / 1,885,016 records; 6,389 sessions / 423,956 messages. Session-touch SQL attempts: 11529. One-minute load: 74.4–146.5; system compressor: 21.23–27.93 GiB. Child CPU including probes: 101.73 s; peak child RSS: 5.74 GiB.
- codex: 7,120,921,596 payload bytes / 1,057,739 records; 326 sessions / 179,926 messages. Session-touch SQL attempts: 786. One-minute load: 88.2–227.1; system compressor: 21.51–25.43 GiB. Child CPU including probes: 48.08 s; peak child RSS: 3.68 GiB.

Both live runs exited cleanly after canonical and search readiness, with zero missing/incomplete sources, checkpoint conflicts, parse hard errors, oversized records, unidentified files, vanished files, or truncated discovery. Compression-start events were absent before readiness. Existing metadata-only pending sessions remain represented. Exact events, counters, readiness polling brackets, and completion checks are in `/private/tmp/rosetta-apply-opt-20260925/live/`.

The code demonstrably eliminates repeated work, but does not establish a fixed startup SLA under memory/CPU pressure. The frozen pair did not show a Claude apply-wall-time improvement, despite lower redundant work. The new live result must not be presented as a code-only speedup from 322 s application: the current host load is substantially lower, and scheduling, memory faults, source growth, and filesystem-cache state are not controlled.

After all checks passed, the six disposable benchmark data trees were removed, reclaiming 31.50 GiB. Native sources, the frozen snapshot, release binaries, and measurement/equality evidence remain intact.

## Operation-level investigation — 2026-09-25, evening

This pass profiles the unchanged candidate `811bd845bf1b9d4b97eb33a89b92d36739bba45bda5ce5f69a6fc62958066fef`. It identifies a missing cold-schema index, expensive JSON materialization and byte-wise framing, and severe scheduling contention. It makes no production code changes and does not claim a controlled week-over-week regression attribution.

Evidence and reusable diagnostic scripts are in `/private/tmp/rosetta-deep-profile-20260925/`. Both agents ran serially against their full native histories with fresh isolated application state, no renderer, and the same release executable. Filesystem caches were not evicted. Native histories remain mutable. There were 1,890,261 Claude records / 6,195,905,328 payload bytes and 1,058,262 Codex records / 7,124,456,241 payload bytes.

### Measurement method

The harness combines existing nested wall timers, HTTP readiness observations, daemon-only `libproc` CPU/I/O/footprint counters at a requested 100 ms cadence, and native `sample` windows of four seconds at five-millisecond intervals. It retains original and demangled call trees. Stack percentages below are **inclusive sampled thread occupancy, not on-CPU percentages**; descheduled threads and blocking syscalls are visible and categories can overlap. Only sample commands whose entire lifetime fits inside the reported phase enter the aggregate, avoiding attribution errors from delayed symbol setup/processing. Short Codex phases have limited sample coverage.

On this Apple M1 Max, the CPU counters are Mach ticks, not nanoseconds. `mach_timebase_info` returned 125/3; conversion was checked against Python's process CPU clock: 300.05 ms versus 300.60 ms. [Apple's kernel](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/thread.h) defines the runnable timer to include running time; [task accounting](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/task.c) sums it over threads. Subtracting user plus system CPU gives accumulated runnable waiting **across threads**, which must not be added to or directly subtracted from elapsed time.

Diagnostic instrumentation adds overhead. Daemon CPU below excludes profiler/probe child processes. Native sampling, resource probes, a tiny symbol-demangler build, and a roughly 0.05 CPU-second synthetic query experiment ran during diagnosis. This is not an ordinary unsampled benchmark. All 29 tracked candidate source hashes remained unchanged. The longest telemetry gap was 2.09 s for Claude and 0.19 s for Codex; phase CPU boundaries therefore have sampling/event-arrival uncertainty.

### Full-stage elapsed times

| Stage, seconds | Claude | Codex |
|---|---:|---:|
| Discovery | 36.67 | 0.19 |
| Metadata | 88.39 | 0.33 |
| Read / parse / normalize / capture | 368.84 | 89.68 |
| Closed-plan preparation | 4.92 | 2.04 |
| Apply / reconcile | 103.20 | 14.67 |
| Finalization | 33.37 | 4.02 |
| Durable publication | 11.24 | 3.23 |
| RAM release / reopen | 0.80 | 0.05 |
| Other setup / coordination | 10.09 | 0.45 |
| Canonical ready, observed | 657.50 | 114.65 |
| Live catch-up | 8.96 | 1.17 |
| Search / remaining handoff | 57.42 | 2.52 |
| **Full ready, observed** | **723.89** | **118.35** |

The process counters explain why wall time alone is misleading:

| Interval | Claude wall / CPU seconds | Codex wall / CPU seconds |
|---|---:|---:|
| Metadata plus surrounding setup | 93.83 / 4.34 | 0.42 / 0.17 |
| Capture | 368.84 / 51.74 | 89.68 / 34.55 |
| Apply / reconcile | 103.20 / 31.34 | 14.67 / 7.96 |
| Finalize / publish / release, including coordination | 47.59 / 7.82 | 7.35 / 2.61 |
| Search after catch-up | 57.26 / 19.36 | 2.52 / 1.18 |
| **Whole run to full readiness** | **723.89 / 122.53** | **118.35 / 48.15** |

Claude accumulated 667.79 runnable-wait thread-seconds; Codex accumulated 70.37. One-minute host load ranged 289.85–813.74 during Claude and 230.41–279.99 during Codex. A host snapshot recorded 1,474 processes, 357 runnable processes, `fseventsd` at 132.7% CPU / 9.40 GiB RSS, and other active Node, Claude, renderer, and Rust compiler processes. These are observations of competing work, not proof that one specific process caused all delay. No unrelated processes were stopped. System compressed-memory occupancy ranged 15.97–32.84 GiB during Claude.

The same candidate's preceding unsampled Claude run reached readiness in 155.85 s, with 101.73 child CPU seconds including small probes. The new run uses 122.53 daemon CPU seconds while elapsed time exceeds 723 s. Different source growth, memory behavior, cache state, diagnostic overhead, and host conditions prevent a code-only comparison, but the directly measured runnable waiting establishes scheduling contention as a large contributor to the current twelve-minute result.

### Operations consuming the time

These nested timings overlap outer stages and parallel ledger work. They must not be summed into an end-to-end total.

| Operation, seconds | Claude | Codex |
|---|---:|---:|
| JSON parsing, normalization, canonical conversion | 144.32 | 40.30 |
| Source read, framing, rolling content checksum | 134.10 | 35.81 |
| Planner control transactions / checkpoints | 37.76 | 0.83 |
| Foreground ledger work and queue waits | 24.58 | 11.95 |
| Ledger worker framing / checksums | 40.54 | 11.03 |
| Ledger worker buffered writes | 15.17 | 32.57 |
| Ledger worker flush | 4.10 | 4.44 |
| Canonical application inside prepared-runtime apply | 70.36 | 3.74 |
| Reconciliation preparation | 12.70 | 5.88 |
| Reconciliation cleanup | 4.36 | 0.62 |
| Message writes, nested inside canonical application | 20.70 | 2.35 |
| SQLite quick-check / foreign-key validation | 17.73 | 2.62 |
| Index creation / ANALYZE | 9.66 | 0.96 |
| Usage and USD bucket rollup | 5.90 | 0.38 |
| SQLite commits across cold work | 2.37 | 0.06 |

**1. Confirmed SQL defect: a provenance guard scans every session.** `store/src/apply.rs::cursor_has_projection` checks nine provenance tables for each new source. Its session branch uses `title_src_cursor`. Every other branch has its index in the base DDL, but `session_title_src` exists only in `BULK_CREATE_INDEXES`, which fresh cold stores run at finalization. Consequently, both the RAM planner and canonical cold writer execute `SCAN session` during ingestion, despite the function's comment promising indexed access. With 9,105 Claude sources and 6,392 sessions, repeated full-table checks are material.

An isolated experiment using the exact repository DDL and guard SQL, 6,389 synthetic sessions, and Python SQLite 3.51.2 reproduced the problem. The unmodified schema used **19,302 VM steps per check**; adding the existing index to the scratch database reduced that to **138**, preserving the false result. Two hundred queries used 49.70 ms versus 0.414 ms thread CPU. This is a query-local experiment, not an end-to-end speedup. Files `cursor-guard-query-plan.json`, `cursor-guard-probe.json`, and `guard_probe.py` preserve the evidence.

In 19 clean Claude capture sample windows (13,629 foreground samples), planner control work occupied 12.61% and this guard alone occupied 9.59%. In ten apply windows (7,074 main-thread samples), it occupied 19.27%. These support prioritizing the missing index without treating the sampling ratios as exact saved seconds. The defect predates the USD work: the title guard and index date to `90fab80` on 2026-08-30; it is not established as the sole cause of a recent regression.

**2. JSON is materialized into an owned value tree, then deserialized into typed records.** Both adapters' `raw::{transcript,rollout}::parse_line` call `serde_json::from_str::<Value>` and then `serde_json::from_value` for known records. This is a second structural deserialization, not necessarily a second scan of the original text. Internally tagged/flattened records also buffer content. Parsing pays allocations and copies for fields that later contribute no canonical row, and drops those trees afterward.

Capture samples attributed 37.43% / 47.96% to parsing plus normalization, including 22.37% / 29.60% in the raw parsers, for Claude / Codex. Allocator leaves occupied 14.43% / 9.45% and memory-copy leaves 9.89% / 8.72% of capture samples across operations. A direct typed/selective parser is a high-value candidate, but must preserve malformed-known-record errors, unknown-field drift, usage, replay state, and canonical equality. The lossless raw ledger can continue to retain fields that need not be materialized in the parser.

**3. The “read” timer contains a serial byte loop.** `capture/src/tailer.rs::frame` processes every byte with a delimiter test and FNV-1a chain update, then allocates/copies record payloads. `read_bounded` also opens/stats the file, checks continuity, rereads the prefix anchor, and rechecks handle/path stability per batch. The sampled native tailer instructions occupied 20.81% / 28.21% of capture; tailer syscall leaves occupied only 9.16% / 9.52%. This disproves interpreting the entire read timer as disk wait.

The hottest sampled offsets, `read_bounded+1428` and `+1440`, were matched against the exact release binary. They land in the loop containing `ldrb`, `eor`, `mul`, and delimiter comparison. `tailer-assembly.txt` and `tailer-self-offsets.json` retain that proof. A faster content-chain design is a candidate, with checkpoint-version and rewrite-detection compatibility work; merely replacing newline scanning leaves the serial checksum dependency. Earlier delimiter-only experiments in this report did not establish a robust speedup.

**4. Reconciliation, row encoding and allocation remain significant.** Claude's apply stage spent 70.36 s in canonical application, 12.70 s preparing reconciliation, and 4.36 s destroying committed reconciliation data. Stacks show B-tree fact/key lookup, tool-patch folding, payload copies, JSON serialization plus BLAKE3 fingerprinting, SQLite B-tree/pager work, and deallocation. Message-write paths occupied 27.28% of Claude apply samples, reconciliation preparation 12.21%, and cleanup 7.73%. These sampled ratios are not a partition of the nested wall timers.

Peak daemon physical footprint was **6.60 GiB for Claude and 3.44 GiB for Codex**. At the end of capture it was 3.37 / 1.41 GiB, before the canonical build's peak. Whole-runtime normalized state plus canonical/reconciliation state amplifies memory use. Existing ownership-transfer improvements help, but additional transient allocation reduction is still worthwhile under memory pressure. This is not evidence of a memory leak. The BLAKE3 `compress_in_place` symbol seen in stacks is a hash operation, not ledger zstd compression.

**5. Metadata repeats session searches and repository lookups.** `pipeline/src/meta.rs::merge_session` linearly searches the growing session vector. `settle_projects` calls the filesystem-backed project resolver per session, without a per-cwd cache. Claude has 5,829 sessions with a cwd but only 175 distinct cwds and 104 projects. Four clean metadata windows attributed 9.68% to session-vector merge, 24.81% to project resolution, and 38.66% to head/tail or document-snapshot reads. Metadata used only about 4.34 CPU seconds including setup but took 93.83 elapsed seconds in this run. A session-id map and per-pass normalized-cwd/repository cache are concrete smaller optimizations.

**6. Codex experiences raw-ledger backpressure.** The worker's 32.57 s buffered-write timer overlaps parsing; only 11.95 s is charged to foreground ledger work/waits. Across nine clean capture windows, the Codex foreground was in ledger sends for 10.09% of samples. Its ledger worker was at write syscalls for 32.23% and idle receiving for 51.15%, indicating bursty backpressure rather than continuous CPU saturation. Claude's worker was idle receiving for 82.16% of capture samples. Larger/unbounded queues would increase memory exposure; queue/buffer changes need a bounded-memory benchmark before claiming improvement.

**7. Search and validation are a separate serial tail.** Claude indexed 129,886 search documents. Polled search stages took approximately 32.02 s indexing, 10.96 s index creation, 13.53 s validation, and 0.67 s publication/handoff. Search samples show SQLite `pread`, FTS insertion/tokenization, metadata-index construction, and FTS integrity checking; 48.56% of search-thread samples were read syscall leaves. Canonical row decoding exists but was not the leading sampled search operation. Codex search was only about 2.52 s after catch-up and had no fully contained four-second sample window. The explicit canonical and FTS integrity checks must not simply be removed to manufacture a shorter ready time.

### Data volume and unnecessary work

The current filter already skips payload ingestion of **32,117 inventory-only Claude files** and 45 Codex files. Discovery still traverses directories and stats matched inventory-only files; adapter-driven directory pruning is a potential improvement. Raw records greatly outnumber final messages because tool results, usage, branches, and metadata fold into existing canonical rows; that ratio is not proof those records are disposable.

Claude's published SQLite file is 1.13 GiB: the message table occupies 574.18 MiB, `drift_seen` 134.69 MiB, and `ingest_cursor` 79.66 MiB, with the remainder largely indexes and other canonical tables. There are 343,316 distinct drift identities but only 791 drift groups. Applying drift cost 3.65 s during prepared application. Compact stable dedupe identities and grouped updates merit investigation, while preserving exact deduplication/count semantics; they are below the missing index and parsing in this run's priority order.

Final checkpoint JSON totals 76.89 MB for Claude (largest 1.11 MB) and 2.65 MB for Codex. Claude `checkpoint_state` appeared in 2.02% of capture samples; this does not include every encoding/CAS operation. It is measurable repeated work, but the evidence does not rank it above raw parsing, framing, or the SQL guard. `database-stats.json` records aggregate counts/sizes only, without transcript content.

Usage/USD finalization was 5.90 / 0.38 s and is not responsible for the multi-minute capture/apply delay. Ledger compression performed no cold-start work: Claude's start event was observed at 723.984 s, after full readiness at 723.887 s; it stopped with zero rewritten segments and 0.0027 CPU seconds. Codex emitted no compression-start event before shutdown.

### Priority and completion

The first implementation target is moving `session_title_src` into the base cold DDL and asserting the cold query plan. Next are avoiding full intermediate JSON trees, reducing framing/checksum CPU, and removing repeated metadata lookups; reconciliation allocation and drift bookkeeping follow. Codex ledger waiting is a distinct measured target with an approximately 12 s foreground ceiling in this run. Host scheduling pressure must be controlled or at least measured during every subsequent comparison: code-level improvements alone do not establish a stable elapsed-time SLA under the observed contention.

Both runs exited zero after canonical and search readiness, with zero missing/incomplete sources, hard parse errors, unknown records, oversized records, checkpoint conflicts, exhausted retries, vanished/unidentified files, or truncated discovery. Claude retains 550 metadata-only pending sessions, as before. One pre-readiness Claude HTTP status request timed out; the final readiness observation bracket was 0.127 s. The source-code hash check passed, and native canonical/search validation passed. No production behavior was changed by this investigation.

`methodology.json`, `summary.json`, `proc-analysis.json`, `stack-aggregate.json`, `database-stats.json`, and `completion-checks.json` summarize the evidence; per-agent logs, counters, native samples, demangled stacks, and original binary/source hashes remain beside them.

After validation and aggregate storage analysis, the two disposable benchmark output directories were removed, reclaiming 15,328,626,658 bytes (14.28 GiB). `cleanup.json` records the exact paths. Native histories, the ordinary app data, executable, and diagnostic evidence were retained.
