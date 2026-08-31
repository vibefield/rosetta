# Slice-budget sweep

`sweep.py` is a standard-library Python harness that runs `rosettad serve` once per
starter-slice budget over one frozen agent-data snapshot and compares the milestone
timings, so the slice sweet spot is chosen from measurements instead of intuition. It
requires Python 3.11 or newer and nothing else. It contains no adapter, parser, schema
copy, or ingest path of its own — it only starts the real daemon and reads its stdout.

## The hard rule

**Comparative runs are only meaningful against the same frozen, permission-read-only
snapshot directory passed as `--home` — never a live agent home directory.** Budgets are
compared across separate processes minutes apart; if the source data can change underneath
them, the differences measure the data, not the budget. This is the repository's
frozen-pair performance discipline: a cold-write comparison cites one checksummed source
snapshot, one machine, and one configuration.

The harness refuses two ways of pointing it at live data:

- **`--home` that *is* the home directory**, recognised by stat identity (`st_dev`,
  `st_ino`) rather than by comparing paths. Path spelling lies: a macOS firmlink alias
  such as `/System/Volumes/Data/Users/you` and a Linux bind mount are not symlinks, so
  `resolve()` hands them back unchanged even though they are the same directory as `~`.
  Device and inode are not fooled, so every spelling of the home directory is refused.
- **A snapshot that links back into the live home** — the classic
  `ln -s ~/.claude snapshot/.claude`, which sweeps mutating data while looking frozen.
  The `--home` argument itself and its *immediate children* are checked; an entry that is
  a symlink whose target is the live home or anything beneath it is refused by name.
  Targets are matched by walking the resolved path's ancestors and stat-ing each one, not
  by string prefix, so an alias of the home directory is caught here too.

**Both checks are floors, not proofs.** Symlinks deeper than the immediate children are
not scanned, an unlistable directory is not checked at all, and nothing here can tell a
frozen snapshot from any other writable directory: the checks catch the common accident,
they do not establish that the data holds still. Freezing the snapshot is yours to do.
A symlinked snapshot is refused even when its target is itself frozen — pass the real
path instead.

**Results are machine-local evidence and are never committed.** Write the CSV and any
per-run logs outside the working tree, and quote them in a commit message or a document
with the machine, configuration, and snapshot identity attached.

## Usage

Run from the repository root:

```sh
python3 tools/slice-sweep/sweep.py --home /path/to/frozen-snapshot \
  --budgets 64k,128k,256k,512k,1m,0 \
  --out /private/tmp/slice-sweep.csv
```

| flag | meaning |
|---|---|
| `--home DIR` | required; the frozen snapshot handed to `serve` as `--home` |
| `--budgets LIST` | comma-separated `--slice-budget` values, run in the order given (default `64k,128k,256k,512k,1m,0`) |
| `--timeout SECS` | per-run limit, default 3600; the run's whole process group is killed and the run marked failed |
| `--out PATH` | write the CSV here; without it only the markdown table is printed |
| `--keep` | retain each run's temporary projection directory and tee each run's output to `slice-sweep-<budget>.log` beside `--out` |
| `--debug-build` | build and run the debug profile instead of `--release` |
| `--agent KIND` | restrict to one agent kind; repeatable, and replaces `--all-agents` |

A budget is a whole number of bytes with an optional `k` or `m` suffix (`65536`, `64k`,
`1m`). `0` disables slicing and is the control run — include it. Two budgets that resolve
to the same byte count are rejected, so every row has a distinct identity.

Each budget gets a fresh `mkdtemp` whose `projection.sqlite` must not pre-exist, and the
harness runs, from the repository root:

```
cargo run --release -p rosettad -- serve --db <tmpdir>/projection.sqlite \
  --home <HOME_SNAPSHOT> --all-agents --ticks 0 --slice-budget <value>
```

The daemon is built once up front (`cargo build -p rosettad`) so that compilation is
charged to no run's wall clock. The temporary directory is removed afterwards unless
`--keep` is given.

## Print contract

`sweep.py` reads these lines from the serve process's merged stdout and stderr. They
appear in this order over a run, and `elapsed_s` is seconds since the serve process
started, printed with two decimals:

```
milestone: starter-drained elapsed_s=<float> files=<uint> paused=<uint>
milestone: cold-drained elapsed_s=<float>
milestone: canonical-ready elapsed_s=<float>
milestone: search-ready elapsed_s=<float>
```

A run that fails or is interrupted may lack the later milestones, and `starter-drained`
is absent when slicing is disabled with `--slice-budget 0`. Unrelated lines are
interleaved and ignored; a repeated milestone line keeps the first occurrence.

## Output

The CSV and the markdown table carry the same nine columns, in the order the budgets
were given:

`budget,starter_s,cold_s,canonical_s,search_s,wall_s,files,paused,ok`

`budget` echoes the token as typed. `files` and `paused` come from `starter-drained` and
are therefore empty for `--slice-budget 0`. A milestone the run never printed is an empty
CSV cell and a `—` in the table. `wall_s` is this harness's own external clock around the
whole `cargo run`, so it is always larger than `search_s` — it includes cargo's own
startup and the daemon's shutdown, and it is the fallback measurement when a run prints
no milestones at all. `ok` is `true` only when the serve process exited 0 within the
timeout; a run can be `ok` and still be missing milestones, which is exactly what the
empty cells are there to show. The process exits non-zero only when every run failed.

## Tests

```sh
python3 -m unittest discover -s tools/slice-sweep
```

Budget parsing, milestone parsing, and row assembly are pure functions with no process
handling, so the suite covers them over sample text and never starts a daemon. The
`--home` refusals are covered too: `resolve_home` takes the live home as an injectable
argument defaulting to `Path.home()`, so the tests exercise both refusals — and the
unscanned depth beyond the immediate children — against temporary directories, never a
real home.
