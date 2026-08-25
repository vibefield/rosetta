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
   canonical model, replayable normalization, capture, and storage. The DB is a
   disposable projection of the ledger.
3. **Services** — Rust protocol/server crates and one daemon (`rosettad`), with
   localhost REST + WebSocket:
   `/v1/search`, `/v1/observe`, `/v1/metrics`.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before writing code.

```sh
# Verify the data/tool boundary and generated artifacts.
python3 -m tools.groundtruth check
python3 -m unittest discover -s tools/groundtruth/tests

# Re-infer schemas from committed fixtures; no package download or agent run.
python3 -m tools.groundtruth farm claude-code --schemas-only
python3 -m tools.groundtruth farm codex --schemas-only

# Verify Rust runtime crates.
cargo test --workspace
```
