# tools/ui-bench

One UI readiness run: the production Agent History app (`clients/app`, unchanged) over a
frozen snapshot, read by a versioned script, measured by an injected probe, written as one
`ui-run/1` JSON. What the metrics mean, their roles and the pair rules are in
`docs/ARCHITECTURE.md`, "Cold-build measurement" → *UI readiness*. This file is how to run
it and read the output.

## Before a run

```sh
npm run build -w @vibefield/rosetta-agent-history   # the production renderer, clients/app/dist
cargo build -p rosettad --release                    # or use a pinned binary
```

The snapshot is the frozen R1 corpus (0444 files, 0555 directories, `SHA256SUMS`,
`snapshot.json`), kept outside the repository; `run_ui.mjs` verifies it with
`tools/r1-bench/verify_r1_snapshot.js` before every run. The R1 kit's scripts are CommonJS
and the repository is ESM, so `tools/r1-bench/package.json` (`{"type": "commonjs"}`) lets
them run in place; no hashed R1 file changed.

## Run

```sh
node tools/ui-bench/run_ui.mjs --rosettad BIN --snapshot SNAP --out OUT_DIR \
  --script reader-v1|passive-v1 [--keep] [--timeout-s 600] [--dpr 2] \
  [--idle-min 75] [--guard-wait-s 1800] [--no-idle-guard] [--min-free-gb 25]
```

`OUT_DIR` must not exist and must be outside the repository and the snapshot. A run takes
about a minute on the R1 snapshot. An Electron window (1500×940 at device scale factor
`--dpr`, default 2, the MacBook's) opens and is driven by trusted CDP input; leave the mouse
alone while it runs. In order, the driver:

1. refuses below `--min-free-gb` free disk, then waits for two consecutive 2 s `top` samples
   with CPU idle ≥ `--idle-min` (up to `--guard-wait-s`, then proceeds and records it);
2. verifies the snapshot; hashes the binary, the renderer dist (sha256 tree digest), the
   script and every harness file;
3. starts `rosettad serve --home SNAP/home --db OUT/db/rosetta.sqlite --addr 127.0.0.1:0
   --json --announce --announce-dir OUT/announce --profile` behind `drain.mjs`, which reads
   its stdout and stderr straight to files with receipt stamps (no `--agent`: consent is
   the roll call's; no `--ticks 0`: the run outlives publication and search);
4. launches `electron --force-device-scale-factor=N tools/ui-bench/app-entry.mjs --frame
   1500x940` with `AH_DAEMON` (the scale factor must be on Electron's own argv; a run whose
   page reports another DPR fails with `dpr-mismatch`);
5. runs the script; ends when it is done and the daemon printed `canonical-ready` and
   `search-ready` (or `search-failed`), or at `--timeout-s`;
6. collects the probe export, quits the app, SIGINTs the daemon (SIGKILL to its pid if it
   has not exited after 60 s), then, once its exit was seen, digests the db read-only
   (`digests.mjs`: the R1 logical and cursor-order SQL, `ledger verify --json`,
   `r1_ranked.py`, stopped with SIGINT so it reaps its warm daemon) and reads from it every
   session, project, runtime and timeline id, native id, title, summary, cwd, name and root;
7. derives the metrics (`metrics.mjs`), passes the document through the privacy gate
   (`privacy.mjs`, `ui-run-1.schema.json` plus a scan against those values, the run's
   paths and the query texts) and writes `OUT/ui-run.json`. A refused document is not
   written; `run.log` lists the errors by path, naming a refused key only by its length
   and a hash prefix. If the db cannot be read the gate fails closed.

Without `--keep` the db, the Electron profile and the raw daemon, Electron and digest logs
are deleted on every exit path — success, failure, or SIGINT/SIGTERM to the driver alone,
which first stops Electron, the daemon and any digest child (the db only once the daemon's
exit was seen). What stays is `ui-run.json`, `digests/` (digest outputs and counts only)
and `run.log`. Electron also quits by itself if the driver's IPC channel goes away.

Exit 0 means: `outcome` complete, probe self-test passed, `run.invariantsPass` (I1 and
H7's selection jumps and aim misses) and privacy passed; 1 otherwise; 2 on a harness error;
130/143 after SIGINT/SIGTERM. A script step that timed out (a `timeout-*` mark) makes the
outcome `incomplete`.

## Files

| File | Role |
|---|---|
| `run_ui.mjs` | Single-run driver; `buildRunDoc` assembles the document |
| `drain.mjs` | The daemon's dedicated stdout/stderr reader |
| `app-entry.mjs` | Electron main entry: fresh userData (and temp), throttling off, focus emulated, worktree/git, favicon, file-browser and browser-preview IPC and `shell.openExternal` denied, every request but the app's own scheme, its renderer files and the run's daemon cancelled, CDP probe registered before the first app load (its binding honoured only from the main frame), then `clients/app/electron/main.mjs` |
| `reader-context.mjs` | The scripts' API in the main process: probe events, CDP `Input.*`, rects |
| `scripts/reader-v1.mjs`, `scripts/passive-v1.mjs` | The versioned reader scripts |
| `probe.js`, `probe-worker.js` | The injected page probe and its Blob worker |
| `daemon-lines.mjs`, `metrics.mjs` | Daemon stdout parsing; metric derivation |
| `privacy.mjs`, `ui-run-1.schema.json` | The whitelist gate and schema |
| `build_schema.py` | Generates the schema: `python3 -I tools/ui-bench/build_schema.py tools/ui-bench/ui-run-1.schema.json` (edit the generator, never the JSON) |
| `digests.mjs` | Read-only equality digests via the R1 kit |
| `test/` | `node --test tools/ui-bench/test/*.test.mjs` (synthetic fixtures only; `probe-dom.test.mjs` runs `probe-dom.electron.mjs`, the probe in a hidden Electron page with planted breaches) |

## Reading `ui-run.json`

- `identity`: binary, dist, snapshot, script, harness hashes, machine, window, DPR
  (requested and measured), declared deviations, the enrolled set. Two runs are comparable
  only when these match as the pair rules require.
- `run`: `outcome` (`complete`, `incomplete`, `timeout`, `failed`) and `outcomeReasons`,
  `selfTestPass`, `invariantsPass`, the guard, every denied host port and blocked request
  (`stubbed`), the script's input counts and marks.
- `clocks`: the import clock's zero (`t0Kind`, normally `consent-granted`), how daemon time
  was mapped (`method`), the renderer and main-process offsets and the estimated error.
- `metrics`: `H1`–`H8`, `Hall`, `I1`, `D1`–`D3`, each with its `role`. Every `…Ms` is on
  the import clock unless its name says lag. Each H metric's `reached` map says, per value
  a pair compares, `yes`, `not-reached` (the state never happened, so the value is null:
  `H5` when no row flipped while drawn) or `not-applicable` (the script declares it never
  exercises it, as passive-v1 does for every selected-project leg, or its frozen set is
  empty). A value whose precondition never happened is null, never 0. Each I1 check
  carries `exercised`, how much it examined. `H6.report.probeMainMs` is the probe's own
  JavaScript self-time on the page's main thread.
- `daemon`: the daemon's own milestones (`milestonesS` from serve start, `milestonesMs` on
  the import clock), tree-admission totals, the memory peak.
- `equality`: logical, strict, `sourceSetDigest`, ranked and UI hit digests, and counts.
  A pair is void unless logical, `sourceSetDigest`, ranked and UI hit digests are equal.
- `probe`: the probe's raw export (timings on the page clock, counts, enums), so metrics
  can be re-derived.

Ids, titles, paths and query text never leave the page; the probe returns rectangles to
the script, and the document holds counts, sizes, timings, enums and digests only.
