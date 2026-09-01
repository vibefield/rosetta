# rosetta

Multi-agent data ingestion engine. Ground-up rewrite of `p008/spaghetti`.

Three jobs: full-text search over agent transcripts, live session/metadata observation
for the agent-view app, and live distilled metric feeds for runtime monitors
(e.g. `p008/chopsticks`).

Three layers, strictly separated:

1. **Ground truth** — data-only agent directories plus a standard-library Python
   evidence pipeline. Each captured agent has one canonical `groundtruth.json`
   and one generated `groundtruth.html` report.
2. **Runtime** — Rust crates under `crates/`: permissive per-agent parsers, one
   canonical model, replayable normalization, capture, and storage.
3. **Services** — the localhost REST + WebSocket layer and generated client contract.

Current implementation status: all nine captured agents have ground-truth packs,
runtime adapters, and CLI registration. The generic pipeline ingests adapter-selected
JSONL transcripts, rewrite-in-place metadata documents, and SQLite session databases;
it reconciles virtual membership and whole-source deletion, generation-stages rewrites, tails live
changes with polling/audit recovery, and writes the canonical SQLite projection.
Verified inline images are content-addressed in a sibling `<projection>.assets/` directory;
SQLite stores only stable service URLs while the raw ledger preserves the native bytes.
Existing/live `ingest`, `watch`, and `serve` append source records to a checksummed raw
ledger durably before visible projection commits. A normal fresh `serve` binds HTTP/WS first,
publishes bounded project/session metadata from the canonical shared-memory SQLite generation,
then captures and reconciles runtimes in parallel RAM. Only closed runtimes enter that generation;
the final handoff fsyncs and promotes the staged ledger first, then atomically installs the validated
SQLite file. Project/session/timeline reads remain progressive, while global heatmap, usage, and
contribution queries return `not_ready` until canonical publication; detached search starts after
that gate. Finite `ingest` and `rebuild` retain the same private-build/atomic-publication contract.
The app starts `serve --restart-incomplete`: an interrupted noncanonical artifact set is moved
intact under `recovery/incomplete-*`, its enrollment is retained, and the fast fresh path restarts.
Ordinary opens still fail closed on schema mismatch.

The in-process hot tier, subscription hub, localhost HTTP/WebSocket surface, generated
TypeScript protocol package, and backend-neutral search seam are implemented. Production
FTS5 is a detached, disposable sidecar built and repaired by an asynchronous epoch/revision
worker; canonical and search readiness are separate wire facts. A benchmark-only Tantivy
prototype is not a production backend: relevance judgments, product query weighting, and
storage acceptance remain. Consumer migrations and the dedicated metrics stream remain.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before writing code. The 2026-08-28
adapter and client-visibility closure is recorded in
[docs/ADAPTER_AUDIT.md](docs/ADAPTER_AUDIT.md).

```sh
# Verify the data/tool boundary and generated artifacts.
python3 -m tools.groundtruth check
python3 -m unittest discover -s tools/groundtruth/tests

# Re-infer schemas from committed fixtures; no package download or agent run.
python3 -m tools.groundtruth farm claude-code --schemas-only
python3 -m tools.groundtruth farm codex --schemas-only

# Verify Rust runtime crates.
cargo test --workspace

# Profile discovery/meta/cold ingest and inspect the final projection.
cd tools/ingest-bench && npm install && npm start
```

`farm` without `--schemas-only` installs and executes downloaded agent CLIs. It fails
closed unless run inside a container/VM (prefer a runner image pinned by SHA-256
digest), explicitly attested with `ROSETTA_FARM_ISOLATED=1`, or deliberately overridden
with `--allow-host-execution` on a disposable research machine.
