# Rosetta ingest bench

`ingest-bench` is a local Electron workbench for verifying Rosetta below the L3
service boundary. It drives the real `rosettad` binary for discovery, bounded
metadata ingestion, and full cold ingestion, then opens the published SQLite
projection directly through a defensive read-only connection.

It does not contain an adapter, parser, schema copy, or alternate ingest path.
Agent-specific knowledge stays in `groundtruth/<agent>` and
`crates/adapters/<agent>`; all writes still flow through the production L2
pipeline and store.

## Workflow and timing semantics

One run creates a unique directory and executes three fresh daemon processes in
sequence:

| phase | daemon command | output | reported wall time |
|---|---|---|---|
| Discovery | `rosettad discover --json` | inventory events | process start through completed walk |
| Meta ingest | `rosettad meta --db meta.sqlite --ledger meta.ledger --json --profile` | isolated live-mode metadata projection and ledger | process start through bounded skims and metadata commits |
| Full cold ingest | `rosettad ingest --db rosetta.sqlite --ledger rosetta.ledger --json --profile` | atomically published canonical projection and durable raw ledger | process start through FTS, buckets, validation, and publication |

The external wall clock is the headline for each process. The UI also shows
`pipeline::Instrument` counters emitted by that process. Those internal counters
may overlap—especially `apply time` and its SQLite subphases—and must not be
summed into a synthetic total.

Both metadata and full ingest perform their own prerequisite discovery. The
metadata phase intentionally uses a different database and ledger, so it never
warms or mutates the full cold run. Every benchmark directory is new; the app
does not delete or overwrite earlier evidence.

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
- an output parent directory that has room for a metadata projection, full raw
  ledger, and full projection;
- one or more installed agents returned by discovery.

After a successful cold run, the Database tab opens `rosetta.sqlite`
automatically. It provides schema-object navigation, paginated table/view data,
full cell inspection, canonical row-count summaries, and an ad hoc console for
`SELECT`, `WITH`, `EXPLAIN`, and `PRAGMA` statements. Results are capped at 1,000
rows. The SQLite handle is opened read-only, `query_only` and defensive mode are
enabled, and database work stays in Electron's main process.

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
run-<UTC timestamp>-<suffix>/
├── benchmark-report.json   # commands, events, logs, wall time, profile metrics
├── meta.sqlite             # standalone bounded-meta result
├── meta.sqlite.lock        # cross-process writer lock database
├── meta.ledger/            # metadata phase ledger
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

- isolated wall-clock measurements for discovery, metadata, and cold ingest;
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
