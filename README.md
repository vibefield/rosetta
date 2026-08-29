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
Existing/live `ingest`, `watch`, and `serve` append source records to a checksummed raw
ledger durably before visible projection commits. A first cold build uses a private
same-filesystem ledger tree, syncs every segment and directory entry, atomically promotes
that tree, and only then publishes the validated SQLite projection; a missing projection
beside an existing ledger fails closed to `rebuild`. `rebuild` validates and replays the
whole ledger into a private database before atomically replacing the projection. Ordinary
opens still fail closed on a schema mismatch so replacement is never implicit. The
in-process hot tier, subscription hub, localhost HTTP/WebSocket surface, generated
TypeScript protocol package, and a backend-neutral search seam with synchronous FTS5 as
the reference backend are implemented. A benchmark-only Tantivy prototype meets the frozen
build/latency/filter/determinism gates, but it is not a production backend: relevance judgments,
product query weighting, storage acceptance, live repair, and crash recovery remain. Consumer
migrations, a detached search worker, and the dedicated metrics stream remain.

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
