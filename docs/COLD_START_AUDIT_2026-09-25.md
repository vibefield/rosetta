# Cold-start audit and optimization plan — 2026-09-25 (evening session)

Goal set by James: the app's fresh cold start takes more than 10 minutes; bring it under 2 minutes.
This document records what the cold start does today, where the time goes (CPU versus waiting),
the experiments run tonight, and the optimizations those experiments support. It complements
`COLD_START_INVESTIGATION_2026-09-25.md` (the day's earlier campaigns) and should be folded into
`ARCHITECTURE.md §7` once the decisions at the end are made.

All measurements are from this Apple M1 Max / 10-core / 64 GiB host on the evening of 2026-09-25.
Unless stated otherwise they are single runs on a heavily loaded machine and are **indicative, not
frozen before/after pairs** in the sense of the CLAUDE.md performance rule. CPU-seconds
(user+system of the daemon process) are quoted next to wall time because they are far more stable
under load than wall time and are the honest measure of code efficiency.

## 1. Bottom line

| What | Measured |
|---|---:|
| James's app run this morning (SamsungRed output, renderer attached) | ~12 min to search-ready |
| James's app run at 18:03 (load 300–800, renderer attached) | ~23 min |
| Same binary, all nine agents, no renderer, output on the internal disk, load 130–210 | **311 s** (canonical 271 s) |
| Same binary, same configuration, two hours later at load 62→32 | **122 s** (canonical 114 s, CPU 150 s) |
| Daemon CPU for that run | 177 CPU-s (0.57 cores average) |
| Claude Code alone, same binary, back-to-back runs | 174 s and 256 s (CPU 103 / 107 s) |
| Claude Code alone, LTO build | 104 s (CPU 88 s) |
| Claude Code alone, parallel-capture prototype, 4 workers | **72 s** (CPU 89 s) |
| Claude Code alone, prototype 3 (all Tier 1 items below that were prototyped) | **56 s** (CPU 75 s) |
| Codex alone, prototype 3 | **22 s** (CPU 43 s) |
| **All nine agents, prototype 3, three runs interleaved with the current binary** | **81.5 / 87.1 / 86.0 s** (CPU 136–138 s) versus **122 / 170 s** (CPU 150–160 s) for the current binary in the same window |

Two facts explain the gap between "10+ minutes" and what the code needs:

1. **The host gives a single thread 0.3–0.6 of a core right now.** Load average was 234–484 on
   10 cores at 18:30 with 0 % idle, 17–33 GB in the memory compressor, 28 GB of swap in use, 43 days
   of uptime, 72 login sessions, 13 `claude` and several `codex` processes (some 40 days old),
   `fseventsd` at 115 % CPU with a 17 GB footprint, and Ghostty at 23 GB. A probe with N busy
   processes obtained 0.31 cores (N=1), 0.91 (N=2), 1.62 (N=4) and 2.29 (N=8). Every serial phase
   of the daemon is stretched 2–7× by this, and the daemon's 5–8 GB retained runtime is paged
   through the compressor (1.36 million page faults during one Claude apply phase).
2. **After the largest capture finishes, 154 s of strictly serial work remains** in the all-agent
   baseline: Claude apply/reconcile 71 s → finalize/publish 44 s → live catch-up 14 s → search
   25 s. That tail alone exceeds the 2-minute budget, and none of it is parallel today.

The whole corpus is 13.3 GB of JSONL (Claude Code 6.2 GB / 1.9 M records, Codex 7.1 GB / 1.06 M
records, Grok 1.0 GB). Parsing and normalizing all of it costs only **~23 CPU-seconds** measured
with the real adapters. The daemon spends 177 CPU-s. Most of the CPU is bookkeeping around the
parse, and most of the wall time is waiting.

## 2. Host and storage findings (no code change needed)

| Finding | Evidence | Consequence |
|---|---|---|
| Load 234–484 on 10 cores, 0 % idle, 156 MB unused RAM, 17–33 GB compressor, 28 GB swap | `top`, `uptime` at 18:30 | A single-threaded phase runs at 0.3–0.6 cores; page-fault storms during apply |
| `fseventsd` 115 % CPU, 17 GB RSS | `top` | Every file write the daemon makes (13 GB plain ledger) feeds this daemon |
| 13 `claude` + several `codex` processes, some 40 days old; Ghostty 23 GB; 69 registered Claude sessions | `ps`, ListAgents | Idle sessions hold memory that would otherwise be page cache |
| SamsungRed 96 % full (45 GB free); `rosetta-agent-tests/runs` = **388 GB** of old benchmark runs (10–16 GB each); `rust-target` 42 GB | `du` | Near-full APFS/SSD writes slower; sequential write measured 429 MB/s vs 1.3 GB/s on the internal disk |
| Ledger is written **plain** (13 GB) on the cold path since the deferred-compression change | ledger segment sizes | 13 GB of writes on the critical path; on SamsungRed that is ≥ 30 s of pure write time, and it doubled Codex capture time in the earlier campaign |

Recommendations that cost no code: reboot or at least close the 40-day-old agent sessions and
Ghostty; delete old `runs/manual-*` directories (hundreds of GB); keep the cold-start output on the
internal disk or a drive that is not 96 % full. The same binary went from 23 min (app, SamsungRed,
loaded) to 311 s (harness, internal disk) to 72 s (prototype, Claude only) tonight — the host is
the largest multiplier and must be controlled before any percentage is claimed.

## 3. What the cold start does today, with measured cost

Pipeline (fresh `serve`, `run_fresh_fast_serve` in `crates/rosettad/src/serve.rs`):

1. **Discovery** — walks every registered agent root with walkdir and matches every regular file
   against every layout glob (`GroundTruthDoc::matching_stream` → `glob_match`, uncompiled: three
   Vec allocations per call). `~/.claude` has 69,722 files; 1.47 million `glob_match` calls per
   discovery, 55 % of them on `plugins/` files that can never match. 3–37 s wall, ~2 CPU-s.
2. **Metadata skim** — serial, one runtime at a time on the main thread; every later runtime waits
   for Claude Code. Per file: open, 64 KiB head + 16 KiB tail, JSON parse of every window line,
   plus an uncached git `project()` probe per session (5,829 probes for 175 distinct cwds). No
   capture starts until all runtimes' metadata is public. 6–94 s wall for **4 CPU-s**; a probe
   reading the same 6,032 heads/tails took 6.1 s single-threaded cold and 1.06 s with 8 threads.
3. **Capture** — one producer thread per runtime; within a runtime files are read strictly one
   after another. Per record: 64 KiB read, byte-serial FNV-1a chain, copy into a Vec, JSON to
   `serde_json::Value` then `from_value` into typed structs (two parses), normalize, move payload to
   the ledger queue (4 slots, one worker thread that copies each payload twice more and blake3s
   it), and per batch a planner SQLite transaction (checkpoint CAS on a blob that reaches 1.1 MB
   for long Claude sessions, plus an **unindexed `SCAN session`** in `cursor_has_projection`
   because `session_title_src` is created only at finalization: ~7 CPU-s per pass for Claude,
   paid twice). Every normalized op of the runtime is retained in RAM until the runtime closes
   (5–8 GB peak footprint). Measured: Claude 63–147 s wall / 49–54 CPU-s; Codex 90–95 s / 35 CPU-s.
4. **Plan preparation + apply** — the sole canonical writer applies whole runtimes serially into
   the shared-memory SQLite: one transaction per source, 51-parameter inserts, a lookup per row,
   deferred tool/usage/edit facts fingerprinted by `serde_json::to_vec` + blake3 (for a retry that
   cannot happen), a full tool-row scan that cannot match in fresh serve, and a usage UPDATE that
   rewrites most Claude agent rows after their INSERT. Claude: 24–71 s wall / 22–33 CPU-s, with
   0.3–1.4 million page faults under memory pressure.
5. **Finalization + publication** — usage-bucket fold (4–6 s), 12 deferred indexes + ANALYZE
   (7–15 s), then `PRAGMA quick_check` + `foreign_key_check` (**25–28 s** on today's 1.68 GB
   projection; the CHECK constraints are re-evaluated on every row because they were ignored during
   load), then a page-by-page backup of the RAM database to disk (6 s), fsync, rename. 44 s in the
   all-agent baseline.
6. **Live catch-up** — before search starts, each runner re-walks the whole root (1.06 million
   glob calls), re-reads all 9,105 Claude cursor rows and JSON-decodes their checkpoints (77–94 MB
   of text the daemon itself just wrote) to compare `observed_len` with the file size. 2–32 s.
7. **Search** — the FTS5 worker is woken only after the first runner's catch-up; it re-reads
   canonical through a 2 MiB cache, fully decodes and validates each TimelineEntry, inserts into a
   normal-content FTS5 table, builds seven metadata indexes (six unused) + ANALYZE ("optimizing"),
   then runs an FTS integrity-check that re-tokenizes everything plus a `quick_check`
   ("validating"). 8–57 s. A SQLite-only rebuild of the same 157k documents costs 2.3–3.5 CPU-s.

Per-phase attribution of the all-agent baseline (internal disk, load 132→214):

| Phase | Wall s | CPU s | Cores obtained |
|---|---:|---:|---:|
| Discovery + metadata (all) | 17.0 | 5.4 | 0.32 |
| Capture: Codex + small agents (Claude in parallel) | 95.5 | 77.1 | 0.81 |
| Capture: Claude tail (Codex applying) | 39.4 | 30.1 | 0.77 |
| Claude plan preparation + wait | 4.6 | 3.1 | |
| Claude apply + reconcile | 70.9 | 32.7 | 0.46 |
| Finalize + publish | 43.9 | 11.6 | 0.26 |
| Live catch-up (all) | 14.2 | 8.8 | 0.62 |
| Search build | 25.1 | 8.0 | 0.32 |
| **Total to search-ready** | **310.6** | **176.8** | 0.57 |

## 4. Experiments run tonight and what they established

Harness: `scratchpad/harness/coldrun.py` (adapted from the day's campaign harness: milestones,
stage profiles, 250 ms process CPU/RSS/fault/disk counters, `/api/status` polling, stop at
canonical + search ready). Micro-benchmarks: `scratchpad/parsebench` (links the real adapter and
capture crates). Evidence directories are listed in §8.

| # | Hypothesis | Result |
|---|---|---|
| E1 | Parallelism buys CPU share under this load | N busy workers obtained 0.31 / 0.91 / 1.62 / 2.29 cores for N = 1 / 2 / 4 / 8. Real `ingest_line` over 24 files: 1→8 threads 4.42 s → 1.48 s wall (3.0×). |
| E2 | Parse/normalize is the dominant CPU | **No.** Full `ingest_line`: Claude 484 MB/s (1.23 CPU-s per 593 MB), Codex 717 MB/s. Whole corpus ≈ 23 CPU-s of 177. serde `Value` alone: 777 MB/s Claude; sonic-rs `Value`: 2.5 GB/s (3.3× faster, 5 vs 53 allocations per record); direct typed `from_str` only 5 % faster than the current Value+`from_value` — the cost is in typed deserialization and string allocation, not the tree. |
| E3 | Non-parse capture overhead is large | Tailer alone 578 MB/s (FNV + copy); tail + plain ledger 390 MB/s; tail + zstd-3 ledger 209 MB/s; tail + ingest + plain ledger 213 MB/s (Claude) / 252 MB/s (Codex) ⇒ ~57 CPU-s for the corpus. The daemon's capture uses ~92 CPU-s ⇒ ~30 CPU-s of planner SQLite control transactions, checkpoint encoding and instrumentation. |
| E4 | Metadata skim is latency-bound | 6,032 heads+tails: 6.1 s cold single-threaded, 1.06 s with 8 threads, 0.81 s with 32; the daemon spent 94 s wall for 4.3 CPU-s on it under load. |
| E5 | Finalization cost is mostly validation | On a copy of today's projection: quick_check 28.4 s, foreign_key_check 4.6 s, ANALYZE 4.3 s, all 30 indexes 13.5 s (message_* dominate), backup API 6.2 s, VACUUM INTO 10.9 s. |
| E6 | The FTS build floor is small | 157k documents / 83 MB of prose from Python: insert 5.3 s, integrity-check 1.1 s, quick_check 1.3 s, optimize 0.4 s; sidecar 166 MB. The daemon takes 8–57 s because of decode, the 2 MiB canonical cache, waiting for catch-up, seven indexes + ANALYZE and the double validation. |
| E7 | LTO / codegen-units=1 | Claude-only CPU 103 → 88 s (−15 %); wall 174 → 104 s (partly load). The workspace has no `[profile.release]` today. |
| E8 | mimalloc | No CPU gain (91 s vs 88 s with LTO alone); dropped. |
| E9 | Parallel per-file capture (prototype, production code paths, same ledger/planner/cursor semantics) | Claude capture wall 63.4 s (1 worker) → **19.2 s (4 workers)** at equal CPU (48.8 vs 47.3 s); 8 workers on a busier host 41.9 s with the commit thread saturated (21 s planner SQLite + 18 s ledger-queue wait). Whole Claude start 131 s → 72 s back-to-back. |
| E10 | Checkpoint blobs | Claude: 9,105 cursors, 77 MB total, p99 119 KB, max 1.1 MB (`adapter_state.responses`); re-encoded and CAS-compared on every batch in the planner, re-decoded in full by catch-up. Cumulative serialization ≈ 0.5 GB for Claude — a few CPU-s, minor next to the planner transactions themselves. |
| E12 | Adapter audit probe over the live histories (single thread, load 62–150) | `ingest_line` 32.1 s Claude / 13.3 s Codex wall, of which the intermediate `serde_json::Value` parse-and-drop is 20.4 s / 9.1 s (about two thirds). A one-pass typed `from_str` measured 0.76× / 0.83× of the current parse with zero Ok/Err disagreements over 2.02 M records. **18 % of Claude records (342,216) emit a drift op** because Claude Code writers 2.1.257–2.1.283 add fields the census lacks (`attachment.rendered` 192k, `assistant.apiBlockIndex` 121k, `perTurnEffort` 94k, `message.input_transformations` 85k, `advisorModel` 62k, `serverClassifierRequest` 61k, `wireToolInputs` 31k, …); that is the 343k `drift_seen` rows / 135 MiB / 3.7 s of store work. Unprojected payload that is still fully materialized: Claude tool results 3.65 GB (59 % of bytes) and 411 MB of thinking signatures; Codex command completions 2.75 GB, compaction history 0.65 GB, encrypted reasoning 0.41 GB. |
| E11 | Record-type census (structure only) | Claude bytes: `user` 71 % (tool results, avg 27.6 KB), `assistant` 15 %; ~65 % of records are tiny ledger-only types (hook_success, progress, …). Codex bytes: `event_msg/item_completed` 49 %, `response_item/custom_tool_call_output` 25 %, `compacted` 14 % (avg 721 KB). |

Prototype 2 adds four environment-gated changes to prototype 1 (deferred quick_check/FK check,
lazy catch-up checkpoint decode, search wake before catch-up, placeholder planner checkpoints for
intermediate batches). Prototype 3 adds the `session_title_src` index at cold-load time, retains
payload batches without a per-batch planner transaction (one terminal control batch per file), and
a configurable ledger queue. Their runs are recorded in §5 as they complete.

## 5. Prototype results

(Claude Code only unless noted; sequential runs on the same host; load varied 50–130.)

| Run | Capture wall | Apply wall | Canonical ready | Search ready | Daemon CPU |
|---|---:|---:|---:|---:|---:|
| Current binary (`811bd845`), run 1 | 69.0 | 63.2 | 160.5 | 174.3 | 103.1 |
| Current binary, run 2 | 146.5 | 33.8 | 221.5 | 256.1 | 106.9 |
| LTO + codegen-units=1 | 49.4 | 24.3 | 92.7 | 104.4 | 87.7 |
| Prototype 1, 1 worker (control) | 63.4 | 24.3 | 108.7 | 131.1 | 97.5 |
| Prototype 1, 4 workers | 19.2 | 23.8 | 61.5 | 71.9 | 89.1 |
| Prototype 1, 8 workers (busier host) | 41.9 | 44.5 | 115.4 | 147.6 | 112.7 |
| Prototype 2, 8 workers + all flags | 21.4 | 33.4 | 72.8 | 89.3 | 94.2 |
| **Prototype 3, 8 workers + no planner transactions + session index** (load 29→23) | **14.8** | **18.1** | **48.9** | **56.1** | **75.3** |
| **Prototype 3, all nine agents, 8 workers per runtime** (load 23–60) | 31.1 (all) | Claude 20.2 | **72.0** | **81.5** | 136.2 |
| Prototype 3, Codex alone | 13.9 | 3.1 | 20.7 | **21.9** | 43.1 |
| Prototype 3, all nine agents, 4 workers per runtime, run 1 | 33.0 | Claude 22.2 | 78.0 | **87.1** | 137.8 |
| Current binary, all nine agents, interleaved (load 38→60) | 91.6 | Claude 36.3 | 157.5 | **170.4** | 160.4 |
| Prototype 3, all nine agents, 4 workers per runtime, run 2 (load 60→34) | 33.6 | Claude 20.9 | 77.4 | **86.0** | 137.7 |
| **Prototype 2, all nine agents** (load 44–90) | Codex 33.4 / Claude 50.9 | Codex 15.3 / Claude 37.8 | **116.3** | **126.6** | 165.2 |
| **Current binary, all nine agents, repeat at load 62→32** | Codex ~40 / Claude 57.9 | Claude 28.7 | 113.6 | **122.4** | 150.5 |

**Read the all-agent pair carefully.** The same current binary took 311 s at load 132–214 and
122 s at load 62–32 two hours later: host load, not code, explains the largest part of James's
10–23 minute starts. Under the lower load the prototype-2 all-agent run (127 s) was *not* faster
than the current binary (122 s): with eight workers per runtime, nine runtimes started 72 capture
threads, the per-runtime commit threads saturated (62 s of ledger-queue waiting summed across
runtimes, capture CPU 111 s versus 97 s), and the gains from deferred validation (finalize 8.4 s
versus 13.8 s) and lazy catch-up were eaten by that contention. Prototype 3 (no per-batch planner transactions, `session_title_src` at load time, larger ledger queue)
cut the Claude-only start to 56 s with 75 CPU-s: capture 14.8 s, canonical apply 9.7 s instead of
23–30 s (the session-scan fix), finalize 5.8 s, search 4.8 s. Its output matched the baseline's
structure: 9,117 files, 0 hard errors / incomplete / missing / conflicts, 5,848 indexed + 550 pending
sessions, 117 message updates; row totals differ only by the corpus growth between runs. With all nine
agents (still 8 workers per runtime) it reached search-ready in 81.5 s with 136 CPU-s, against 122 s and
150 CPU-s for the current binary under comparable load: capture 31 s versus 58 s, Claude apply 20 s
versus 29 s, finalize 9 s versus 14 s. Its ledger-queue wait was still 40 s summed across runtimes, so
the ledger writer remains the next bottleneck. A 4-worker all-agent pair is recorded below as it
completes. The design conclusion stands either way: the worker count must
be a global budget (about the core count) rather than a per-runtime constant, and the ledger
writer must stop being a single thread per runtime that copies and hashes every byte.

The prototypes reproduce the sequential path's ledger frames, cursor CAS chain, source
generations and planned batches by construction (the commit thread applies worker output in
arrival order; only fresh cursors take the parallel path). Row counts differ across runs only
because the live histories grow between runs; a frozen-snapshot 17-table equality check is still
required before any of this merges, per the repository's performance rule.

## 6. The optimization plan

Ordered by expected effect on the critical path. "Saves" are estimates on a quiet host for the
all-agent start, derived from the measurements above; the loaded-host effect is larger.

### Tier 1 — contained fixes, each independently shippable

| # | Change | Where | Saves | Evidence |
|---|---|---|---|---|
| 1 | Create `session_title_src` before the cold load (base DDL), and skip `cursor_has_projection` when both generations are 0 | `store/schema.rs`, `store/apply.rs:1700` | ~14 CPU-s (Claude), planner + writer | store audit: 6.6 s per pass at 6,380 sessions, 0.22 s indexed |
| 2 | Move `quick_check`/`foreign_key_check` off the readiness path: install the projection, wake search, then validate the installed file on a background thread and demote to a rebuild on failure (or enforce CHECKs during load so the pass is a page walk only) | `store/lib.rs::finish_bulk_load`, `serve.rs` | 25–33 s wall | E5; ARCHITECTURE keeps the gate, so this is a placement change, not a removal |
| 3 | Wake the search worker before live catch-up; make catch-up lazy (decode a cursor only when size or mtime changed; on a fresh start pass `observed_len` from the planner) | `serve.rs::drive_live_runners`, `pipeline/live.rs::poll` | 10–30 s wall | discovery/search audits; catch-up 14 s in the baseline |
| 4 | Search build: build connection cache 256–512 MiB, canonical reader `mmap_size`, keep only `search_document_session`, drop the database-wide ANALYZE, run the FTS integrity-check and `quick_check` after publication (or concurrently on two connections) | `search/src/worker.rs`, `sidecar.rs` | 10–20 s wall | E6; six unused indexes are 13 % of the sidecar |
| 5 | Planner control plane: no per-batch SQLite transaction; one terminal control write per file; full checkpoint only on the terminal batch | `pipeline/stream.rs`, `store/lib.rs` | ~15–20 CPU-s, all on the capture critical path | prototype 1 profile: 21 s planner apply on the commit thread |
| 6 | Compile layout globs once (pre-split patterns, prune `plugins/` and other unreachable subtrees with `filter_entry`, carry the matched stream index in `ScannedFile`) | `adapter-support/src/lib.rs`, `capture/discovery.rs`, `pipeline/live.rs` | 1–30 s wall (discovery + every poll) | discovery audit: 10–18× faster replica |
| 7 | Parallel metadata skim (bounded pool over files, merge in inventory order) and memoized `project()` per cwd; start each runtime's capture as soon as its own metadata is public | `pipeline/meta.rs`, `serve.rs` | 5–15 s wall (up to 90 s under pressure) | E4; 5,829 git probes for 175 cwds |
| 8 | Release profile: `lto = "fat"`, `codegen-units = 1` | `Cargo.toml` | −15 % CPU everywhere | E7 |
| 9 | Apply path: drop retry fingerprints on the closed-bulk path, skip `load_existing_openers` in fresh serve, settle usage owners in RAM before the INSERT so agent rows are written once, cache the drift cursor path per batch | `store/apply/bulk_reconcile.rs`, `store/apply.rs` | 5–10 CPU-s | store audit |
| 10 | Refresh the Claude Code census and rebuild `groundtruth.json` (writers 2.1.257–2.1.283 added about ten fields), type the new fields, and dedupe drift per source generation so a new writer release cannot amplify per record | `tools/groundtruth`, `groundtruth/claude-code/`, `adapters/claude-code/src/raw/transcript.rs`, `store/apply.rs:531-604` | 342k fewer drift ops per cold start, 135 MiB less `drift_seen`, ~4 CPU-s, and a drift alarm that means something again | E12 (adapter audit) |

### Tier 2 — structural (the changes that reach the target)

| # | Change | Why it is needed | Status |
|---|---|---|---|
| A | **Parallel per-file capture** inside each runtime (worker pool reads, frames, normalizes; one commit thread appends to the ledger and records control state in order) | Capture is the largest phase and is embarrassingly parallel across files; the serial version cannot use more than one core per runtime | Prototype validated: 63 → 19 s for Claude at equal CPU. Needs: the commit thread relieved (Tier 1 #5), frame packing + blake3 moved into producers or per-worker segments so one ledger writer is not the limit, a byte budget on in-flight batches |
| B | **Stream rows into the canonical store during capture** instead of retaining whole runtimes as Rust object trees; keep only the tool/usage reconciliation index in RAM; write resolved tool rows and flip the runtime visible at close | Removes the 24–71 s serial apply from the tail, the 5–8 GB peak footprint and the page-fault storms; the visibility rule ("only a complete, clean runtime is public") is unchanged because readiness is still a per-runtime commit | Design only. Ordinals: the precomputed-ordinal path can be replaced by provisional ordinals plus the existing renumber, or by a per-session ordinal allocator on the commit thread. This is the one change that needs a product decision (see §7) |
| C | **Overlap the search build with finalization**: build the sidecar from the finalized RAM generation while backup/install run, or at least wake it at publication | The disposable index has no reason to wait for catch-up or for the disk copy | Wake-before-catch-up is in prototype 2; RAM-source build is a small protocol change (`Fts5SidecarSearch` only knows the disk path today) |
| D | **Compress the ledger in the workers** (zstd level 1–3, per-worker segments) instead of writing 13 GB plain on the critical path | Cuts cold-path writes ~5× and removes the 10 GB of temporary disk; the CPU (≈ 0.3 s per 100 MB at level 3) is spread over the pool | Needs the per-worker segment numbering the ledger index already tolerates |

### Tier 3 — later

- One-pass typed parse instead of `Value` then `from_value` (measured 0.76× / 0.83× of the current
  parse, zero disagreements over 2.02 M records; the one behaviour change is duplicate known keys
  becoming a hard error), and a block deserializer that reads the block `type` first to avoid serde's
  untagged deep clones. sonic-rs would be ~3× on the tree parse alone, but the parse is only ~13 % of
  today's CPU; both belong after Tier 1/2.
- Zero-copy or skip types (`RawValue`) for heavy fields nothing projects: Claude `toolUseResult` and
  tool-result content (3.65 GB, 59 % of Claude bytes), thinking signatures (411 MB), Codex
  `item_completed` command output (2.75 GB, read only on failure), `replacement_history` (647 MB),
  `encrypted_content` (405 MB). This removes more than half of all bytes from the allocator.
- Type-peek before the full parse for Claude's declared-untyped record kinds: 53 % of records but
  16 % of bytes; safe only with a full-line validation pass; Codex is not safe to peek.
- Bound `ClaudeCodeState.responses` (it only grows; 254 MB serialized for 72 MB of final state) and
  decode checkpoints in the live poll with a view that skips `adapter_state`.
- Codex output rescans (`visit_embedded_json` over 866 MB, the async-text check allocating 1.47 GB,
  Edit/Write diffs built twice): about 3.5 s combined.
- Contentless/external-content FTS5 and integer surrogate ids in `search_document` (sidecar
  −60 %).
- `drift_seen` keyed by digest instead of a JSON string (375k rows, 135 MB in the projection).

## 7. Budget: can it reach 2 minutes?

Critical path model for the all-agent start (13.3 GB), using tonight's measured CPU costs and the
cores actually obtained on this host:

| Stage | Today (loaded host) | With Tier 1 + A + C (loaded host) | Quiet host |
|---|---:|---:|---:|
| Discovery + metadata | 17 | 5–8 | 3 |
| Capture (largest runtime) | 135 | 25–40 | 12–18 |
| Apply + reconcile (serial) | 71 | 25–35 | 15–20 |
| Finalize + publish | 44 | 15–20 | 10 |
| Catch-up + search | 39 | 5–10 (overlapped) | 5 |
| **Total** | **311** | **75–115** | **45–60** |

Measured tonight rather than modelled: prototype 3 with all nine agents took 81.5, 87.1 and 86.0 s to
search-ready (canonical 72–78 s) in three runs interleaved with the current binary, which took 122 and
170 s in the same window. The prototype is also far less sensitive to load: its three all-agent runs
spread over 6 s while the baseline's spread over 48 s (and 311 s earlier in the evening). Its phase
breakdown — metadata 10–12 s, capture 31–34 s, Claude apply 20–22 s, finalize 9–10 s, catch-up and
search 8–10 s — shows the remaining budget: the serial Claude apply (Tier 2 B), the ledger writer
(40 s of queue wait summed across runtimes; Tier 2 D), and the still-serial metadata skim (Tier 1 #7).

With Tier 2 B (streaming apply) the serial apply mostly disappears from the tail and the loaded-host
figure drops to roughly 60–90 s. Under 2 minutes is therefore reachable on this host once its load
is under control and Tier 1 + A + C land; it is comfortable on a normal host. The August 31
reference (`COLD_INGEST_IO_FLOOR.md`) reached canonical readiness in 55 s for 9.95 GB on this
machine, which corroborates the quiet-host column.

## 8. Decisions for James

D1 and D2 were tried on 2026-09-27; see §10 for the outcome. D4 was measured on 2026-09-28; see §11.

- D1 Validation placement: validate the installed projection after publication (Tier 1 #2) and
  the search generation after readiness (Tier 1 #4), with failure demoting to rebuild. Both checks
  are kept; only their position changes.
- D2 Streaming apply (Tier 2 B): rows enter the canonical RAM store as they are produced, invisible
  until the runtime's readiness commit. This keeps the visibility rule but changes the "retain the
  whole runtime in RAM" implementation the current text of CLAUDE.md describes.
- D3 Ledger codec on the cold path: compress in producers (Tier 2 D) versus keep plain + deferred
  compression. Plain costs 13 GB of critical-path writes and 10 GB of temporary disk.
- D4 Release profile: adopt fat LTO / codegen-units=1 (build time roughly doubles).
- D5 Host hygiene: the 388 GB of old runs on SamsungRed and the long-lived idle sessions.

## 9. Evidence and how to rerun

Scratchpad root: `/private/tmp/claude-501/-Users-jamesyong-Projects-project100-rosetta/0e890dc8-da08-4fbd-b8f5-9374af9c06c8/scratchpad/`

- `harness/coldrun.py` — cold-run harness; `harness/attrib.py` — per-phase CPU/fault attribution.
- `evidence/<label>/{result.json,proc.jsonl,status.jsonl,stdout.log}` for every run in §5.
- `parsebench/` — parse/capture micro-benchmarks (`run1.out`, `run2.out`).
- `dbprobe/` — finalize and FTS probes on a copy of today's projection.
- `reports/{capture,store,search,discovery,adapters}.md` — the five code audits with file:line evidence
  (the adapter probe sources are in `scratchpad/ingest-probe/`).
- `variant/tree`, `tree-proto2`, `tree-proto3` — the prototype source trees (environment-gated:
  `ROSETTA_CAPTURE_WORKERS`, `ROSETTA_DEFER_VALIDATE`, `ROSETTA_LAZY_CATCHUP`,
  `ROSETTA_EARLY_SEARCH`, `ROSETTA_CHEAP_PLANNER_CKPT`, `ROSETTA_PLANNER_NOCTRL`,
  `ROSETTA_LEDGER_QUEUE`); binaries under `variant/target-*/release/rosettad`.
- `NOTES.md` — running notes with every number above.

Nothing in the repository's tracked code was modified by this session; this document is the only
new file. All benchmark outputs were written to the internal disk and deleted after each run.

## 10. Outcome of D1 and D2 (2026-09-27)

Both were implemented in separate worktrees on a snapshot of the then-uncommitted working tree,
checked against the frozen snapshot (projection digests equal to the baseline in every canonical
table, message ordinals included), and measured in interleaved live all-agent cold starts: three
per variant, same host and hour. Means in seconds unless noted.

Campaign 1, load 14–88:

| | Baseline | D1 | D2 |
|---|---:|---:|---:|
| Canonical ready | 132.5 | 136.1 | 119.6 |
| Search ready | 143.5 | 147.1 | 131.6 |
| Daemon CPU | 161.9 | 175.5 | 153.9 |
| Peak footprint, GiB | 8.6 | 9.1 | 5.0 |

Campaign 3, load mostly 8–23, with the session-index fix (Tier 1 #1) on top of D2:

| | Baseline | D2 | D2 + index |
|---|---:|---:|---:|
| Canonical ready | 110.2 | 91.9 | 89.0 |
| Search ready | 118.9 | 102.9 | 98.4 |
| Daemon CPU | 147.5 | 143.1 | 127.9 |
| Peak footprint, GiB | 8.7 | 4.7 | 4.8 |

- **D2 and the session index are merged** (`8d43b30`, `cc9606c`). Claude Code's apply after
  capture fell from about 30 s to 9 s because rows now land while capture runs. That moved writer
  work into the capture window and lengthened Claude's capture by 4–18 s depending on load. The
  index removed most of that extra CPU: Claude's planner apply fell from 10.2 s to 1.3 s. The
  stage-by-stage history remains on `cold/d2-streaming-apply` and `cold/d2-session-index`.
- **D1 is not merged.** Moving the canonical `quick_check` and foreign-key check behind
  publication removed about 5 s from finalization. The check then read the installed file from
  disk in 12–20 s, overlapped catch-up and search, and cost about 14 extra CPU-seconds. No
  end-to-end gain was measurable at these loads. The implementation remains on
  `cold/d1-post-publication-validation`.
- **Kept from D1: `quick_check` on a read-only connection skips CHECK constraints.** SQLite does not
  parse CHECK constraints when a database is opened read-only (verified on 3.46 and 3.51), so an
  audit must use a read-write connection with `PRAGMA query_only = ON`. The cold path validates on
  its read-write RAM connection and is unaffected. Equality reports from earlier campaigns that
  opened outputs read-only did not evaluate CHECK constraints.
- Early tool-row resolution at file close (D2's skipped stage B3) is provable only with a runtime
  check that a session has exactly one source; it would move the remaining 3.4–3.9 s of Claude
  tool-row writes out of admission. D3–D5 remain open.
- Evidence (plans, reports, digests, per-run evidence and binaries):
  `/Volumes/SamsungRed/rosetta-agent-tests/runs/d1d2-campaign-20260927/`.

## 11. Outcome of step 1, the contained Tier 1 batch (2026-09-28)

Three parallel branches, integrated as one batch over `6a56afd`: compiled and pruned layout walks,
walking every root concurrently, a pooled metadata skim merged in inventory order, one git probe per
cwd per pass, and each runtime's capture starting as soon as its own metadata is public (Tier 1 #6
and #7); search woken at publication, poll evidence held in RAM, and a search build with a larger
build cache, one metadata index, no `ANALYZE` and a single `quick_check` (Tier 1 #3 and #4); a
journal-free staging copy and an in-memory usage-fold sort (finalize). Every frozen run of the
batch produced a projection digest equal to `main`'s; workspace tests 1,186 passed.

Interleaved frozen pairs (campaign `cold-t1-20260927`, load 80–135), `main` → batch:

| | `main` | Batch |
|---|---:|---:|
| Metadata ready | 23.3 s | 3.4–3.5 s |
| Claude capture starts | 24.3 s | 3.5 s |
| Live catch-up | 16.6 s | 0.4–1.6 s |
| Search ready after canonical ready | 36.5 s | 6.1–9.8 s |
| Retired instructions to readiness | 730–771 G | 656–685 G |

End-to-end wall times in the same campaign spread over 100–617 s with host load 66–325 and are
dominated by capture, which this batch does not change (steps 2 and 3 below).

- **Not taken:** enforcing CHECK constraints at write time (`ARCHITECTURE.md` records why); running
  `quick_check` and `foreign_key_check` concurrently (the shared-cache mutex serializes them); SQLite
  sorter threads for index builds; memory-mapped canonical reads for the search build (a read I/O
  error would become SIGBUS).
- **D4, release profile: not adopted.** Fat LTO with one codegen unit produced an equal digest, but
  on the frozen workload it retired as many instructions (629–653 G to canonical ready versus
  619–642 G without it) and spent as many cycles (265–320 G versus 270–309 G), while a clean build
  took 26 minutes under load. The hot paths are SQLite's C code and serde, which Rust LTO does not
  reach. The earlier −15 % (E7) came from single runs under changing load.
- **Follow-ups seen during step 1:** a live admission during catch-up still re-runs the runtime's
  whole metadata phase (Hermes admits once on every start; Claude once took 22.7 s under load), and
  a warm start still decodes every checkpoint once on its first poll (1.6–11 s under load).

## 12. Outcome of step 3, taking work off the canonical writer (2026-09-28)

Each runtime's producer now holds a session's tool, usage and file-edit facts until the session's
last roster source is emitted, then resolves them with the admission fold's own code and sends
final tool rows (details already serialized) with that source. Sessions that another source
touches before they complete fall back to the runtime fold. A source naming an already resolved
session fails the start closed; that never happened on the frozen or live corpus, and a failed
start resumes through ledger replay, which does not use this path. Smaller changes: a four-column
usage lookup, freeing consumed rows and plans on a reclaimer thread, and one-pass session
activity for large mirror refreshes. Every Claude and Codex tool row now resolves before
admission; all frozen digests are equal to `main`'s.

- Calm frozen pairs over step 1: canonical ready 55.9–57.1 → 51.0 s, Claude admission 5.5 → 1.1 s,
  Codex 2.6–3.0 → 0.15 s, writer busy 16.1 → 12.1 s, daemon CPU unchanged, peak footprint
  3.4 → 2.55 GiB (live: 5.30 → 3.34 GiB).
- **Measure loaded pairs with care.** On this host, frozen runs at load 70–210 fell into two
  modes whatever the binary: Claude capture about 45 s or about 85–105 s, with every producer-side
  timer (read, normalize, planner apply) doubling in the slow mode. Three frozen and three live pairs
  first pointed against step 3 because it landed in the slow mode each time; a four-way isolation
  run then put the same binary in the fast mode twice (canonical ready 56.9 and 60.1 s against
  67.3 s for step 1) and showed no effect from the reclaimer thread. Compare runs within one mode,
  and prefer retired instructions and per-thread CPU (reported in `--profile` since this step) to
  wall time when the host is loaded.

## 13. Outcome of step 2, parallel capture (2026-09-28)

One process-wide pool of capture workers (cores − 1 by default; `--capture-workers 0` keeps the
serial walk) steps resumable per-file passes one tail batch at a time: read, normalize, checkpoint,
frame and hash. Each runtime's producer became a sequencer that commits worker output strictly in
path order, so the raw ledger is byte-identical to the serial walk's for any worker count (except
the metadata frames' `discovered_at`), and the producer-side tool resolution of step 3 runs on it in
emission order. Ledger file ids became roster keys so workers can checksum finished frames; decoded
entries are unchanged. Look-ahead is bounded by a 512 MiB budget whose over-budget tasks park rather
than block. Every frozen digest and ledger source digest equals `main`'s.

- Calm interleaved means over `main` with steps 1 and 3: frozen canonical ready 57.4 → 47.9 s, live
  87.4 → 71.0 s; search ready 61.8 → 52.0 s frozen, 95.7 → 80.2 s live; the same retired
  instructions; daemon CPU +8 %; live peak footprint 3.41 → 4.57 GiB.
- The pool also halves the cold start's sensitivity to host load: on the frozen sweep canonical ready
  fits about 77.5 + 0.64·load serially and 66 + 0.36·load pooled.
- **What limits it now:** the sole canonical writer's share of the CPU (its CPU is unchanged, but its
  wall time is 1.5–2× its CPU when pooled, and Codex's producer waits 9–18 s on the writer's
  in-flight budget); Codex's longest chain of large files (26–30 s, now the critical path); and
  Codex's resolution burst after its state-database unit.
- **Found while testing, not yet fixed:** a transcript that ends inside a record makes the fresh
  pass end staged, and the whole cold start fails; a record over 32 MiB abandons its first
  generation and admission then refuses the whole build.

## 14. Outcome of step 4, the Claude Code census refresh (2026-09-28)

A census pinned to writer 2.1.283 (the newest of 166 writer versions on this machine) documents 13
transcript fields and 2 subagent-sidecar fields that writers 2.1.257–2.1.283 added; the adapter's raw
types now know them, and none is projected. The census tool also stopped publishing three kinds of
native identifiers (tool-use ids used as map keys, epoch-stamped artifact version ids, per-call block
ids); 41 such values already committed in the 08-28 and 09-03 censuses were migrated out of the
files, although they remain in the repository's history. No fixture was farmed for the new fields:
that needs an isolated runner, and `watch.toml` records the gap.

- Live all-agent, interleaved: drift ops per cold start 566,876 → 41,398; `drift_seen` rows about
  235 MB → 17 MB; the writer's drift apply 3.0 → 0.31 s; retired instructions 1,045 G → 993 G.
- `normalizer_version` stays 30: only drift facts change, and the store never retracts a drift row,
  so a bump would replay every Claude source on the next warm start without removing any. Existing
  projections keep their earlier drift rows until rebuilt.
- A new writer field still costs one drift op and one `drift_seen` row per record until the census
  catches up; bounding that needs a change to what drift counts mean or a schema bump.

## 15. End to end after steps 1–4 (2026-09-28, 05:00 PDT)

Three interleaved live all-agent cold starts of each binary on a calm host (load 4–36; about
698,500 messages), `6a56afd` (after D2) against `d8954d3` (steps 1–4). Means:

| | `6a56afd` | `d8954d3` |
|---|---:|---:|
| Canonical ready | 101.3 s | 61.2 s |
| Search ready | 118.6 s | 69.1 s |
| Metadata ready | 11.2 s | 4.1 s |
| Claude capture ends | 74.5 s | 33.6 s |
| Claude admission | 9.7 s | 1.9 s |
| Live catch-up | 13.1 s | 1.8 s |
| Search ready after canonical ready | 17.3 s | 7.9 s |
| Daemon CPU | 144.8 s | 136.1 s |
| Retired instructions to canonical ready | 1,015 G | 941 G |
| Peak footprint | 4.9 GiB | 4.3 GiB |

Codex is now the longest capture (38–46 s against Claude's 27–33 s): its largest files form one chain
that no worker count shortens, and its sessions resolve only after its state database. On a loaded host
the pooled capture's sensitivity to load is about half the serial walk's (§13).

## 16. Ledger compression in the capture workers, and robustness fixes (2026-09-28)

**Compression (decision D3, still open).** Capture workers can now write the cold ledger as one zstd
frame per tail batch, concatenated into `RLZ1` segments (`--cold-ledger-codec zstd-frames:N`); the
default stays plain. Compressed ledgers are byte-identical for any worker count and decode to the plain
bytes; every digest and ledger source digest equals `main`'s. Live all-agent on a calm host, plain /
level 1 / level 3:

| | plain | zstd-frames:1 | zstd-frames:3 |
|---|---:|---:|---:|
| Canonical ready, internal disk | 53.3 s | 52.1 s | 51.7 s |
| Daemon CPU by canonical ready | 127.7 s | +12.5 s | +20.4 s |
| Bytes written by canonical ready | 17.5 GB | 6.4 GB | 5.8 GB |
| Ledger size | 15.6 GB | 4.4 GB | 3.8 GB |
| Peak footprint | 4.38 GiB | 3.94 GiB | 3.87 GiB |

With plain, the paced background pass then recompresses the ledger for about an hour (107 CPU-s
unpaced, reading 19 GB) and ends at 3.68 GB; a compressed cold ledger leaves it nothing to do. On the
USB SSD a plain ledger made the capture wait about 30 s for the ledger queue (one calm round).

**Robustness.** A fresh (or resumed fresh) start no longer fails when a transcript is caught mid-append:
the closed walk ends that source's generation at its last complete record, and the live runner lands the
rest as ordinary growth. Still open, for a decision: a record over the 32 MiB capture limit fails every
fresh start (the largest on this machine are 26.3 MiB for Grok and 24.5 MiB for Codex, which was 4.85 MiB
a month earlier), and a live append of one is quarantined without being ledgered; the fresh start also
still refuses a whole runtime for any hard error, vanished, missing or unidentified file, which the warm
path tolerates. None of the 233 runs of this campaign hit one of these.

## 17. Decisions of 2026-09-28

- **D3, the cold ledger codec: compress in the capture workers.** Since `d9f632b` a fresh or resumed cold
  build writes `zstd-frames:1` unless `--cold-ledger-codec plain` is given (numbers in §16). On the
  frozen snapshot the projection digest and the ledger's source digest equal the plain build's, and the
  ledger is 1.7 GB instead of about 8.5 GB.
- **Oversized records: ledger first, quarantine in the pass.** A record up to one raw frame (256 MiB less
  its prefix) is captured and ledgered whole; a JSONL record over the 32 MiB projection limit becomes one
  `raw_record` / `over_projection_limit` drift fact instead of being normalized, its generation stays
  clean and seals, and its session is Indexed. Replay applies the same size rule, so a rebuild from the
  ledger reproduces the projection. Live appends and new files now agree with the cold path (no silent
  ledger/projection divergence, no whole-file re-read on every append). Records above the frame limit are
  still refused. One 200 MiB record costs about 410 MiB of transient memory.
- **Census identifiers in history:** the values the census tool used to publish stay in the history of
  the 08-28 and 09-03 census commits; the files themselves are clean.
- **Still open: how strict a fresh start is.** `record_backfill_failures` refuses a whole runtime, and so
  the start, for any hard error, vanished, missing or unidentified file, exhausted retries or
  incomplete file, while the warm path leaves such files to live admission. Recommendation: treat
  transient per-file conditions (vanished, missing, not yet identifiable, retries exhausted) as the warm
  path does, apply the documented dirty-first-generation rule to malformed lines (publish the parseable
  rows, hold that session `Streaming`), and keep refusing only what signals corruption or a bug (a
  truncated discovery walk, a planner conflict, a record over the frame limit). None of the 233 runs of
  this campaign hit one of these refusals.
