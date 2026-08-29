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
