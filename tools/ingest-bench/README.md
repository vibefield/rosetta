# Rosetta ingest bench

`ingest-bench` is a local Electron workbench for verifying Rosetta below the L3
service boundary. It drives the real `rosettad` binary for independently timed
discovery, bounded metadata ingestion, or full cold ingestion, then opens the
published SQLite results directly through defensive read-only connections.

It does not contain an adapter, parser, schema copy, or alternate ingest path.
Agent-specific knowledge stays in `groundtruth/<agent>` and
`crates/adapters/<agent>`; all writes still flow through the production L2
pipeline and store.

## Workflow and timing semantics

Each stage button creates a unique directory and executes one fresh daemon
process. Stages never depend on a previous bench run:

| phase | daemon command | output | reported wall time |
|---|---|---|---|
| Discovery | `rosettad discover --json` | inventory events | process start through completed walk |
| Meta ingest | `rosettad meta --db meta.sqlite --ledger meta.ledger --json --profile` | isolated live-mode metadata projection and ledger | process start through bounded skims and metadata commits |
| Full cold ingest | `rosettad ingest --db rosetta.sqlite --ledger rosetta.ledger --json --profile` | atomically published canonical projection and durable raw ledger | process start through FTS, buckets, validation, and publication |

The external wall clock is the headline for each process. The UI also shows
`pipeline::Instrument` counters emitted by that process. Those internal counters
may overlap—especially `apply time` and its SQLite subphases—and must not be
summed into a synthetic total.

### Focused cold-stage profile

For one reproducible cold-path decomposition, the command-line harness selects
the largest adapter-admitted JSON-lines sources, copies and SHA-256 verifies
them into a permission-read-only private snapshot, and drives the ordinary
`rosettad ingest` path. Selection/copy/hash time is reported separately and is
not charged to cold ingest. The small optional JSON report omits source paths
and checksum values; the temporary snapshot, raw ledger, SQLite projection, and
search sidecar are deleted unless `--work-dir` is supplied.

Build the daemon first, then profile the three largest Claude Code sources:

```sh
cargo build -p rosettad --release
cd tools/ingest-bench
npm run profile:cold -- --agent claude-code --count 3 \
  --report /private/tmp/rosetta-claude-cold-stages.json
```

Use `--binary PATH` when the desired daemon is not in Cargo's configured target
directory. Use `--work-dir NEW_DIR` only when private artifacts are needed for
manual inspection; the directory must not exist yet and must stay out of
version control.

The report is intentionally honest about the current measurement boundary:
`AgentAdapter::ingest_line` combines parse and normalize, while generic
fold/merge is not a standalone stage. Those rows are marked combined instead
of receiving inferred costs. Stage wall clocks and nested profiler timers are
both retained, and nested parent/child timers are never added together.

The Electron workbench exposes the same machinery in its **JSONL audit** tab.
It inventories the selected adapter's admitted JSON-lines sources, ranks them
by size, and preloads the largest local file and its matching stream. A dropdown
provides the next-largest sources, while the file browser still accepts any
local JSONL/NDJSON. The workbench:

- shows file size, torn-tail status, and bounded head/tail records as both raw
  UTF-8 and parsed JSON without loading a large transcript into the renderer;
- materializes a safe path from the selected canonical stream glob, then asks
  `rosettad sources` to verify that the staged file is genuinely admitted;
- freezes and checksum-verifies a read-only snapshot before running the normal
  release ingest path;
- charts all eleven conceptual boundaries, preserving combined/unobservable
  rows instead of inventing timings, and explains both why each boundary exists
  and what the current production implementation actually does there;
- follows one bounded native record through steps 5–8, showing the parsed
  native JSON, emitted `IngestOp` values, enriched `WriteBatch`, private
  pre-finalization SQLite state, and validated post-finalization state side by
  side;
- compares raw source, ledger, SQLite, and FTS5 sidecar sizes and exposes parse,
  drift, message, merge, session, validation, and publication evidence; and
- retains the private snapshot and output artifacts so the resulting projection
  can be opened directly in the Database tab for a full row-level audit.

“Any JSONL” means any file can be inspected. Ingestion still requires a real
adapter grammar: selecting the wrong stream is expected to surface unknown or
hard-error evidence, not silently reinterpret the file.

Metadata and full ingest perform their own prerequisite discovery internally.
Every click gets a new database and ledger, so running meta cannot warm or
mutate a later full cold run. The app does not delete or overwrite earlier
evidence.

## Run it

Prerequisites are Rust, Node 24 or newer, and npm.

```sh
cd tools/ingest-bench
npm install
npm start
```

`npm start` first runs `cargo build -p rosettad --release`. This keeps the
auto-detected daemon in Cargo's configured target directory synchronized with
the JSON diagnostic protocol expected by the Electron app. A manually selected
binary must be built from this checkout.

The app asks for:

- the `rosettad` executable (auto-detected from `cargo metadata` when built);
- the home directory whose agent roots should be expanded and probed;
- an output parent directory that has room for the selected stage's projection
  and ledger;
- one or more installed agents returned by discovery.

After a successful meta run, the Meta data tab opens `meta.sqlite`
automatically and renders a long-scroll project list. Selecting one project
loads only that project's session tree; subagents nest beneath their parent and
expand on demand. Cards include titles, owning agents, roots, repositories,
workspace, timestamps, source counts, and ingest state. After a successful full ingest, the Database tab opens
`rosetta.sqlite` and provides an ad hoc
console for `SELECT`, `WITH`, `EXPLAIN`, and `PRAGMA` statements. Results are
capped at 1,000 rows. Both SQLite handles are independent, read-only,
`query_only`, and defensive; database work stays in Electron's main process.

Useful verification queries include:

```sql
PRAGMA quick_check;

PRAGMA foreign_key_check;

SELECT type, count(*) AS messages
FROM message
GROUP BY type
ORDER BY messages DESC;

SELECT stream_state, count(*) AS sessions
FROM session
GROUP BY stream_state;

SELECT runtime_id, stream, record_kind, error_code, count
FROM drift_agg
ORDER BY count DESC
LIMIT 100;
```

## Run artifacts

```text
run-discovery-<UTC timestamp>-<suffix>/
└── benchmark-report.json

run-meta-<UTC timestamp>-<suffix>/
├── benchmark-report.json   # command, events, logs, wall time, profile metrics
├── meta.sqlite             # standalone bounded-meta result
├── meta.sqlite.lock        # cross-process writer lock database
└── meta.ledger/            # metadata phase ledger

run-ingest-<UTC timestamp>-<suffix>/
├── benchmark-report.json
├── rosetta.sqlite          # final cold canonical projection
├── rosetta.sqlite.lock     # cross-process writer lock database
└── rosetta.ledger/         # final cold raw ledger
```

These artifacts contain private agent history and absolute local paths. Keep the
output directory local and out of version control. A large organic run can need
space for both the raw ledger and the SQLite projection; the workbench never
cleans them up automatically.

## Design and verification plan

Implemented:

- independent wall-clock runs for discovery, metadata, and cold ingest;
- dedicated project/session views for the bounded meta projection;
- newline-delimited JSON framing at the `rosettad` CLI edge;
- live stdout/stderr capture, cancellation, and a durable JSON run report;
- direct read-only SQLite browsing without leaking raw SQL into L3;
- process argument arrays, context isolation, renderer sandboxing, and no shell
  command construction;
- parser, input-validation, and read-only database unit tests.

Deliberately out of scope for this first workbench:

- live watch/event-to-row latency (a separate long-running workload);
- frozen before/after optimization campaigns and equivalence digests (owned by
  the stricter search/ingest campaign harnesses described in the architecture);
- packaging/signing installers; this is repository tooling for local L3
  verification.

## Development checks

```sh
cd tools/ingest-bench
npm run check
npm test

cargo test -p rosettad
cargo fmt --all -- --check
```
