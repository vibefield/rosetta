# tools/ui-bench

One UI readiness run: the production Agent History app (`clients/app`, unchanged) over a
frozen snapshot (or a synthetic fixture home), read by a versioned script, measured by an
injected probe, written as one `ui-run/1` JSON. What the metrics mean, their roles and the pair rules are in
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
node tools/ui-bench/run_ui.mjs --rosettad BIN --home DIR --fixture MANIFEST --out OUT_DIR \
  --script reader-v1|passive-v1 [--idle-guard] [the same options]
```

`OUT_DIR` must not exist and must be outside the repository and the snapshot (or the
fixture home). `--home DIR --fixture MANIFEST` runs over a fixture home that `fixture.mjs`
generated (see "Fixture test"); its identity (`identity.fixture`: spec, seeds, tree
digest, generator sha256) replaces the snapshot's (`identity.snapshot` is then null), it
is verified against its manifest instead of by the R1 kit (`run.snapshotVerify` is null;
a home outside a fixture's place is refused before any file is read: see "Fixture test"),
and the idle guard is off unless `--idle-guard` (declared as `identity.fixture.idleGuard`).
Snapshot runs are unchanged but for `identity.fixture: null` and two more harness hashes. A run takes
about a minute on the R1 snapshot. An Electron window (1500×940 at device scale factor
`--dpr`, default 2, the MacBook's) opens and is driven by trusted CDP input; leave the mouse
alone while it runs. In order, the driver:

1. refuses below `--min-free-gb` free disk, then waits for two consecutive 2 s `top` samples
   with CPU idle ≥ `--idle-min` (up to `--guard-wait-s`, then proceeds and records it);
2. verifies the snapshot (or the fixture home against its manifest); hashes the binary,
   the renderer dist (sha256 tree digest), the script and every harness file;
3. starts `rosettad serve --home SNAP/home (or DIR) --db OUT/db/rosetta.sqlite --addr 127.0.0.1:0
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
| `source.mjs` | The run's input home: the R1 snapshot or a fixture home, verified, with its identity and newest giant |
| `fixture.mjs` | The fixture home: its spec, generation through bench-corpus, mtimes, manifest and verification |
| `drain.mjs` | The daemon's dedicated stdout/stderr reader |
| `app-entry.mjs` | Electron main entry: fresh userData (and temp), throttling off, focus emulated, worktree/git, favicon, file-browser and browser-preview IPC and `shell.openExternal` denied, every request but the app's own scheme, its renderer files and the run's daemon cancelled, CDP probe registered before the first app load (its binding honoured only from the main frame), then `clients/app/electron/main.mjs` |
| `reader-context.mjs` | The scripts' API in the main process: probe events, CDP `Input.*`, rects |
| `scripts/reader-v1.mjs`, `scripts/passive-v1.mjs` | The versioned reader scripts |
| `probe.js`, `probe-worker.js` | The injected page probe and its Blob worker |
| `daemon-lines.mjs`, `metrics.mjs` | Daemon stdout parsing; metric derivation |
| `privacy.mjs`, `ui-run-1.schema.json` | The whitelist gate and schema |
| `build_schema.py` | Generates both schemas: `python3 -I tools/ui-bench/build_schema.py tools/ui-bench/ui-run-1.schema.json tools/ui-bench/ui-pair-1.schema.json` (edit the generator, never the JSON) |
| `pair.mjs` | The pair runner: side files, the pre-run refusals, ABAB interleaving, frozen inputs, the dist swap, swap redos (below) |
| `pair-eval.mjs`, `ui-pair-1.schema.json` | The pair evaluator (gate set (a), void rules, A/A floors), its CLI and the ui-pair/1 whitelist |
| `digests.mjs` | Read-only equality digests via the R1 kit |
| `test/` | `node --test tools/ui-bench/test/*.test.mjs` (synthetic fixtures only; `probe-dom.test.mjs` runs `probe-dom.electron.mjs`, the probe in a hidden Electron page with planted breaches) |
| `test/fixture-run.mjs` | The real-daemon fixture test (`npm run test:ui-readiness`, local-only; below), with `fixture-checks.mjs` (its verdict, unit-tested by `fixture-checks.test.mjs`), `fixture-search.mjs` and `fixture-expected.json`; `fixture.test.mjs` tests where a fixture home may be |

## Reading `ui-run.json`

- `identity`: binary, dist, snapshot (or fixture), script, harness hashes, machine, window, DPR
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

## Pairs and A/A campaigns

`pair.mjs` turns runs into the frozen before/after pair a perf commit cites (CLAUDE.md,
Conventions); `pair-eval.mjs` judges it. `run_ui.mjs` is unchanged: the runner only
schedules it.

```sh
# 1. one side file per side (hashes computed here; edit nothing by hand)
node tools/ui-bench/pair.mjs side --rosettad BIN --dist DIST_DIR --snapshot SNAP \
  [--scripts reader-v1,passive-v1] [--runs 11] [--timeout-s 600] [--idle-min 75] \
  [--guard-wait-s 1800] [--no-idle-guard | --idle-guard] [--min-free-gb 25] [--dpr 2] \
  [--swap-redo-mb 256] > SIDE.json            # or --home DIR --fixture MANIFEST
# 2. an A/A campaign first: the same side twice, report-only, sets the noise floors
node tools/ui-bench/pair.mjs run --aa SIDE.json --out AA_OUT [--order abab|abba] [--dry-run]
# 3. the pair, claims declared before it runs, floors from the A/A
node tools/ui-bench/pair.mjs run --before A.json --after B.json --out OUT [--kind daemon|ux] \
  [--claims CLAIMS.json] [--floors AA_OUT/ui-pair.json] [--order abab|abba] [--dry-run]
# re-evaluate a pair directory exactly as declared (no new options)
node tools/ui-bench/pair-eval.mjs OUT
```

**Sides.** A side names the rosettad binary and the renderer dist (a `clients/app/dist`
build, copied anywhere) with their sha256 (the dist as `run_ui.mjs` hashes it), one source,
the scripts, N runs and run_ui's options. The kind says what may differ (metrics:X7): a
`daemon` pair (R1.5) varies rosettad only and pins the dist; a `ux` pair varies the dist
only and pins rosettad; `--kind` is inferred when omitted. Before any run the runner
refuses (exit 2) a pair whose sides read different sources, run different scripts, run
counts or options, break the kind's rule (both differ, or neither: use `--aa`), whose
declared hashes are not the files', or whose claims name an unknown metric or a script
the campaign does not run. `OUT` must not exist and must lie outside the repository and
the source.

**What it does.** It copies each side's binary and dist into `OUT/inputs/<side>/` and
re-hashes them (a concurrent build cannot change a side mid-campaign), copies the
checkout's own `clients/app/dist` to `OUT/inputs/original-dist`, writes
`OUT/declared.json` (no paths: kind, sides' hashes, the source's identity, scripts,
options, order, claims and floors; its sha256 is the campaign id) and `OUT/plan.json`
(private: the paths). Then, per round, per script, both sides in order (`abab`: A then B
every round; `abba`: B first on odd rounds), it installs the side's dist as
`clients/app/dist` (the app and `run_ui.mjs` read it there; so do not build the renderer
in this checkout during a campaign), records the power source and Low Power Mode
(`pmset`), and runs `run_ui.mjs` into `OUT/runs/NNN-<script>-<side>/` with the side's
frozen binary. run_ui's own guards run before every run: free disk, then the CPU-idle
guard (two consecutive 2 s samples ≥ `idleMin`% idle, up to `guardWaitS`; on by default for
a snapshot, off for a fixture home, and the choice is part of the declared options). A slot
whose swap grew by more than `swapRedoMb` during it runs once more (`…-redo`); the first is
kept, marked discarded. At the end (and on any failure) it restores the checkout's dist,
then evaluates and writes `OUT/ui-pair.json` (privacy-gated: the `ui-pair-1.schema.json`
whitelist and a scan against every path the campaign used) and `OUT/verdict.txt`. SIGINT
or SIGTERM stops the running `run_ui.mjs` (it cleans up its own children), restores the
dist, evaluates what ran (a short campaign is void) and exits 130/143. Exit 0: pass (or
an A/A campaign completed); 1: fail or void; 2: refused or a harness error. Each run keeps
only what `run_ui.mjs` keeps (no db); `OUT/inputs` holds the binaries and dists (about
45 MB a side).

**Void.** A pair is void (no verdict) when any run is missing, not `complete`, failed the
probe self-test or I1, or did not pass the privacy gate; when any run's snapshot (or
fixture) identity, harness hashes, machine, window, DPR, Electron, deviations, enrolled set
or script hash differs from another's or from the declaration, or its rosettad or dist hash
from its side's; or when, within one script, any run's logical TOTAL, `sourceSetDigest`,
ranked ids, UI hit digest, final roots or final ready sessions differ from another's
(logical, sourceSet and ranked must also be non-null). H7's aim misses and selection jumps
do not void a pair: they are a product finding (every reader-v1 run on the R1 snapshot has
one), reported per run.

**Gate set (a)** (the user's decision, 2026-11-03), on these ui-run/1 fields, every one
"lower is better":

| Gate | Fields |
|---|---|
| ui.newest_readable (H2) | `metrics.H2.ms` (painted), `metrics.H2.receiptMs` |
| horizon 24h @newest (H3) | `metrics.H3.h24.receiptMs`, `metrics.H3.h24.paintedSelMs` |
| horizon 7d @newest (H3) | `metrics.H3.h7d.receiptMs`, `metrics.H3.h7d.paintedSelMs` |
| all readable (H-all) | `metrics.Hall.receiptMs`, `metrics.Hall.paintedSelMs` |
| flip→paint p95 | `metrics.H5.p95Ms` |
| inversion | `metrics.D2.inversionCorpusMs`, `metrics.D2.inversionSelectedMs` (count × ms) |
| open-ready p95 (reader) | H4's p95: ready opens' `contentAt − clickAt` from `probe.opens` (metrics.mjs records only p50 and max, so the evaluator re-derives it with the same `pct`) |
| long-task ms | `metrics.H6.byWindow.{streaming,finalizing,post}.blockingMs` |

A gated value is a **guard** unless `CLAIMS.json` declared it a **claim** before the
campaign (`[{"script": "reader-v1", "metric": "H2.ms", "maxRatio": 0.75, "maxP": 0.05}]`;
the metric ids are those of the table in `verdict.txt`). Every other metric (H1, H2's lag,
H4 p50/max, H5 p50/max, H6 longest frames, input and memory, H7, H8, D1–D3, the daemon's
milestones and footprint, run wall) is **report-only**. Per script and metric:
- a guard passes when AFTER's median ≤ BEFORE's median + BEFORE's IQR (inclusive
  quartiles, as the R1 kit's `summarize.median_iqr`; with fewer than four values the IQR is
  max − min). With `--floors`, the tolerance is max(BEFORE's IQR, the A/A floor);
- a claim passes when AFTER's median ≤ `maxRatio` × BEFORE's (without a ratio: below
  BEFORE's median minus the tolerance) and, if `maxP` is set, the one-sided Mann–Whitney p
  (AFTER smaller; normal approximation, tie-corrected) is below it;
- a value is compared only when every run of both sides reached it (`reached: yes`).
  Never reached on either side is `not-reached`, declared unexercised by the script is
  `not-applicable` (neither counts against the pair); reached in some runs and not others
  is `incomparable`, which fails the pair: a state that appears or disappears is a change
  the lead must look at, not noise.

The pair passes when it is not void and no claim or guard fails or is incomparable.

**A/A.** `--aa SIDE.json` runs one side twice (`a1`, `a2`), interleaved the same way. Every
metric is report-only; each gated one gets `floorMs` = max(IQR a1, IQR a2, |median a1 −
median a2|). Pass the A/A `ui-pair.json` to a later pair with `--floors`: its campaign id
and floors go into that pair's declaration.

**Reading `verdict.txt` / `ui-pair.json`.** The first lines give the verdict (PASS, FAIL,
VOID with its reasons, or AA), the campaign id, both sides' hashes and the quiet record
(runs that proceeded over the idle guard, swap redos, the largest swap growth, runs on
battery or in Low Power Mode). Then one row per script and metric: role, each side's
median and IQR, Δ median, the tolerance and the verdict (an A/A shows the floor instead);
then each equality key, equal or DIFFERS. `ui-pair.json` holds the same plus each run's
outcome, I1, H7 invariant, guard and power, and the reached counts behind every row.

**Smoke test (2026-11-03).** One 2+2 `ux` pair on the fixture home (`--home/--fixture`,
idle guard off, declared; the after dist was the before dist plus one inert file), both
scripts, 8 runs in 2 min 37 s: not void, every equality key equal, PASS; and one 2+2 A/A
of reader-v1. These test the runner, not the app: on the fixture the import is over
before the first paint (see below), so H5 is never reached, and two runs a side make the
IQR max − min.

## The MacBook-shape snapshot

`bench-corpus snapshot --shape macbook-shape-v1` (crates/bench-corpus `shape.rs`,
`snapshot.rs`; metrics:X8b) writes a frozen snapshot in the R1 kit's format (files 0444,
directories 0555 with the root, `SHA256SUMS`, a deterministic `snapshot.json` with
`walkInputsDigest`), so `run_ui.mjs --snapshot` and `verify_r1_snapshot.js` take it as they
take the R1 snapshot:

```sh
cargo build --release -p rosetta-bench-corpus
target/release/bench-corpus snapshot --shape macbook-shape-v1 --out /Users/jasper/james-test/frozen/macbook-shape-v1   # ~40 s
node tools/r1-bench/verify_r1_snapshot.js /Users/jasper/james-test/frozen/macbook-shape-v1                           # ~23 s
target/release/bench-corpus snapshot --shape macbook-shape-v1 --out X --plan   # the plan's summary, nothing written
```

It is synthetic (no live data; the same spec and seed give the same bytes and mtimes) and
shaped on the MacBook's structure-only census aggregates (trace of 1301bdb's R1 build): 1,139
claude-code root trees (40 big: largest own source over 64 MiB) with subagent children
(6,923 subagent transcripts, 5,245 sidecars beside them, 550 sidecar-only subagents; 351
single-file trees), 119 projects, 13,857 claude-code files and 8,612 metadata sessions (8,062
on the roster), plus 9 Codex rollouts so both agents are enrolled. NEWEST is the newest
giant, exactly 668,350,304 bytes in 459 files (its 210,808,432-byte root transcript the
largest file). Trees sit in the census's age rings: 24 in the newest day (23 within 1 h; 13
big), 15 more within 7 d (9 big), 14 within 30 d (3 big), 1,086 older (15 big). **Scaled:**
bytes only, never counts. The newest day is kept at full scale (5.68 GB: the giant exactly,
the other trees as synthetic draws summing to the census's newest-day bytes); every older
big tree stays big (its root transcript is 64 MiB + 0.5–4 MiB) but its other files and
every older small tree are a heavy-tailed size × 1/40 (6 KiB floor a transcript). So
7.76 GB on disk (13,866 files) against the MacBook's 21.76 GB; the 7 d and 30 d horizons
and the tail are much lighter than on the MacBook, and the newest day carries 3,323 files
(2,555 there). Not modelled: workflow files, todos and other non-transcript streams, and
nested subagents. A metadata-only `rosettad meta` over it reports 8,612 claude-code
sessions, 119 projects, 13,857 files (2026-11-03). `walkInputsDigest` da20e10f…02fe1,
`manifestSha256` 0e9b1c1e…bb09c (seed 1).

## Fixture test (local-only)

```sh
npm run test:ui-readiness       # npm run build, then cargo build --release --locked -p rosettad
                                # -p rosetta-bench-corpus, then the test over both scripts
node tools/ui-bench/test/fixture-run.mjs [--rosettad BIN --bench-corpus BIN] [--work DIR] \
  [--reader-only] [--expected FILE] [--write-expected] [--keep] [--timeout-s 600]
```

The real release rosettad, SDK, hooks and renderer over a home anyone can regenerate,
asserting invariants and digests only, never a timing threshold.

**What it covers, and what it does not.** Since ux:X7 (the room asks status as soon as
the roll call's grant is taken) the columns are up 130–262 ms after consent, well inside
the import: the first roots read ready at 40–150 ms, the giant (NEWEST) at 1.5–5.7 s,
canonical readiness at 1.9–6.5 s (12 fixture documents, 2026-11-03, on a machine shared
with other builds). Before it, the columns waited for the roll call's 2.5 s status poll
and the whole import was over by H1 (1.7–2.5 s). reader-v1 selects NEWEST's project
0.5–1.0 s after Begin, while NEWEST is Pending: NEWEST is drawn not imported, on top in
"Still importing", and flips in place there (H5's one sample, about 25 ms receipt → paint).
Its first aim at that card often misses: the card is read first at metadata (ux:X1) and
moves about 94 px when the first flips rank another project above it, 100–250 ms later,
before the pointer reaches the list, so no hold keeps it (H7 records the miss). The reader
sees from the probe that its press met another card and aims again once the card is at
rest. Until it did (2026-11-03), the miss left it in another project, whose rows were all
readable, until its horizon step re-selected NEWEST's 7.2–11.4 s later, NEWEST then ready:
no session row was ever drawn Pending (`flips-done-before-first-paint`, and step 3 marked
`newest-never-ready`), which read as if the selected project hid its Pending rows. The test
covers flip *delivery* (every root's flip reaches the UI, after the daemon made it and in
its order), a Pending row's paint and flip, the end state, the selection and list
invariants, privacy and the digests. Whether the reader's selection comes before NEWEST's
flip is timing, so nothing is asserted from the Pending paint: I1 c1's breach class (a
Pending row painted ready) and H5 are recorded, not asserted, and c4, c7 and the
opened-while-Pending face are not reached. Each run records its **regime**
(`fixture-checks.mjs` `regimeOf`; reader-v1's documented one is `pending-painted`, a row
drawn not imported; passive-v1, which selects no project, `flips-done-before-first-paint`,
no row drawn Pending and canonical readiness after H1) and prints it. A run in another
regime (NEWEST flipped before the reader's selection, or the import over by H1) is flagged
with a NOTE, not failed: it is the order of timings no one guarantees, so nothing is
asserted from it. Pausing the cold build (`POST /api/ingestions/{id}/pause`) is no way to
make Pending paint certain either: a resumed
build runs the cross-launch resume path, which withholds every readiness
decision until publication (no per-tree flips, no `newest-giant-ready`), and the pause
would still have to win a race against the giant's flip.

It takes about 32 s and peaks near 0.3 GB of disk (the binaries, the 81 MB home and one
153 MB run db at a time) in a fresh temp directory (`--work DIR` instead; it must not exist
and must lie outside the repository and the home directory). At the end it deletes the
home, dbs and profiles; a db stays only while a daemon may still hold it, and the test says
so. `--keep` keeps all of it (about 0.45 GB); the home is read-only (directories 0555), so
run `chmod -R u+w WORK` before deleting a kept WORK. Without `--rosettad`/`--bench-corpus`
it builds both and takes the exact executables cargo reports (no mtime-picked artifact); it
hashes each as cargo returns, copies it into the work directory and refuses if the copy's
hash differs (a concurrent build of a shared target directory). Exit 0: every check passed;
1: a check failed (each is printed with its reason, and `WORK/report.json` holds every
check, digest, regime and headline value); 2: setup (a precondition, a missing binary, a
failed build, no renderer build); 130, 143 or 129: SIGINT, SIGTERM or SIGHUP. run_ui runs
in its own process group, so a terminal's Ctrl-C reaches only the test, which forwards one
signal; every child stops and the home, dbs and profiles are still deleted.

**Machine precondition.** bench-corpus writes every cwd as `/tmp/bench/project-N`, and the
meta resolver looks for a `.git` at each cwd and at every ancestor
(`crates/pipeline/src/project.rs`, `find_git_dir`). A checkout there would merge or rename
the fixture's four projects and move every digest, so the test refuses (exit 2) while
`/tmp/bench` exists in any form, or a `.git` sits at `/tmp` or `/`.

**Local-only, on purpose.** It needs a release rosettad, the built renderer, Electron with
a display, and macOS (the driver's guards read `top`, `sysctl` and `sw_vers`, and the
digests use `/usr/bin/sqlite3`). CI's one Electron job (`test:room-host` under xvfb on
Linux) has no release rosettad, and no CI job that builds one and runs this has ever been
run. It is not `*.test.mjs`, so the unit suite never runs it.

**The fixture** (`fixture.mjs`, `ui-fixture-v1`). bench-corpus writes every transcript
under `.claude` and `.codex` of one home, all with `--tool-ratio 0.3`:
- `gen claude-code --sessions 16 --lines 4000 --seed 41`;
- `gen claude-code --sessions 1 --lines 36000 --seed 42`, one 71 MB session over the
  daemon's 64 MiB big-tree line;
- `gen codex --sessions 9 --lines 1500 --seed 43`.

That makes 26 files, 80.8 MB (77 MiB), and 26 single-session root trees (bench-corpus
writes no subagent transcripts) over 4 projects that both agents share (each cwd is its
own project, given the precondition above). The test then sets every mtime from a fixed
base (2026-09-01Z): the giant is NEWEST (3 h before it), and the other 25 files take fixed
ages over their sorted paths (4 more within 24 h of NEWEST, 7 more within 7 d, 14 from 9 to
60 days). So 24h@newest holds 5 roots and 7d@newest 12, from both agents. Finally it makes
the tree read-only, as the R1 snapshot is. The manifest beside the home (never inside it)
records the spec's sha256, the generator's sha256 and a tree digest over each file's path,
size, mtime and content. `run_ui --home` refuses, before reading any file, a home that is,
contains or lies inside the home directory or the repository, a manifest not beside it,
and a home holding anything but the spec's data roots.

**What it asserts** (`fixture-checks.mjs`), per script:
- outcome complete, self-test, privacy (the list read from the run's db, and holding at
  least the db's session, native and project ids);
- I1 passes, and the checks that can catch something here examined something: c3 for both
  scripts, and c2 for reader-v1 (its rows are drawn during the import and after it). c1
  is not asserted as exercised (above). c5 and c6 are not either: c5's count is 0 exactly when it already counts a
  violation, and c6's is a constant 1 (a document exists only if the gate passed);
- every H1–H5 / H-all `reached` entry agrees with its value (a number with `yes`, null
  otherwise), and the values the fixture always produces are reached;
- orderings, with the run's own clock error as slack: H1 ≥ the first project frame
  received; H2 painted ≥ H2 receipt ≥ the daemon's flip of NEWEST (the `publishedS` of the
  tree-admission batch that printed `newest-giant-ready`: NEWEST is the newest giant); each
  H3 receipt ≥ its `recent-ready …@newest`; H-all receipt ≥ the daemon's last root-flipping
  batch, and that batch ≤ the last `runtime-readable`; every H5 sample ≥ 0 (from the
  probe's own pairs, vacuous while H5 is not reached). A missing end fails the ordering,
  except passive-v1's H2 painted, which it declares not applicable;
- the harness's newest giant, the daemon's and the fixture's are one tree (tree bytes),
  NEWEST's receipt is the giant's, and each horizon holds the spec's roots for the UI and
  for the daemon;
- the daemon's flips were found in its stdout, which is this run's; the home is unchanged
  after the run; and the digests equal `fixture-expected.json`.

The test runs run_ui with `--keep` to read the daemon's stdout for those two flips, then
deletes what run_ui would have. run_ui's exit code is not the verdict.

What it does not assert, and why:
- Pending paint, as above.
- H-all against `runtime-readable`: that milestone is stamped at the runtime's close, after
  its last tree flip, and has no unrounded JSON twin (two decimals), so the UI can and does
  receive the last flip a few ms before it (−2.3 ms in one run). Its lag is recorded.
- H7's invariant: recorded. Since ux:X1 the first aim at NEWEST's project misses in most
  runs (above): the card moves before the pointer reaches the unheld list. On the R1
  snapshot reader-v1 also misses at the end-of-import reorder (ux:X9). Both are product
  findings; the reader marks `select-newest-project-missed` and aims again.

**Digests.** Pinned:
- logical TOTAL;
- strict_noid **TOTAL_R**, every strict_noid component but `ingest_cursor_file_evidence`. A
  cursor's tail checkpoint encodes the file's device and inode, so that component changes
  whenever the home is regenerated, wherever it is. Within one test run both scripts must
  agree on strict_noid TOTAL too;
- `sourceSetDigest`, ranked ids, the UI hit digest, roots, ready sessions and every count.

The R1 ranked workload and reader-v1's three queries ask for words bench-corpus never
writes, so on the fixture they pin empty hit lists. `fixture-search.mjs` therefore also
digests seven queries of the fixture's own vocabulary (a warm start on the run's db, as
`r1_ranked.py` does), each of which must find something. Nothing pinned depends on the
home's location; the generated files depend only on bench-corpus and the spec, and the
projects on the machine precondition.

**Refreshing the expected digests** (`--write-expected`, written only when every other
check passed; review the diff). It is legitimate when the change is meant to alter what the
fixture produces, and the commit says which:
- bench-corpus's output or the fixture spec (the tree digest moves);
- the canonical model, an adapter or normalization (logical and strict_noid);
- ledger framing of a source (`sourceSetDigest`);
- ranking (the ranked digests);
- what the room draws for a search (UI hit digest).

Never refresh to make a failing invariant pass, or because a digest moved without such a
change: that is the regression this test exists to catch.
