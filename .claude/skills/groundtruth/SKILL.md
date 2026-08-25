---
name: groundtruth
description: Capture and maintain an agent's ground truth (Layer 1) — mock-model dialect, fixture-farm runner, declarative layout, raw Rust types, replay tests, canonical groundtruth.json, and generated groundtruth.html.
---

# Ground-truth capture workflow

Phase 1 is breadth-first across agents. For one captured agent, the finished
state is:

1. `groundtruth/<agent>/watch.toml` — release channel and fixture-proven pin.
2. `groundtruth/<agent>/layout.toml` — declarative stream inventory with honest
   `research`, `observed`, or `fixture` evidence.
3. `groundtruth/<agent>/fixtures/<version>/<scenario>/` — redacted hermetic bytes.
4. `groundtruth/<agent>/sources/` — fixture/census/source intermediates.
5. `groundtruth/<agent>/groundtruth.json` — the only canonical schema truth.
6. `groundtruth/<agent>/groundtruth.html` — generated, committed visualization.
7. `crates/adapters/<agent>/src/raw/` and fixture replay tests — the production
   Rust parser, written against the canonical JSON.

Read `CLAUDE.md`, the agent card in
`docs/groundtruth/RESEARCH_PLAN.md`, and the agent's `watch.toml` first. Use
`groundtruth/codex/` and `groundtruth/claude-code/` as reference data packs.

## Boundary

- `groundtruth/` is data-only. Do not put Rust, Python, or executable scripts in
  an agent directory.
- All evidence-pipeline code lives in the standard-library Python package
  `tools/groundtruth/`.
- All Rust lives under `crates/`; agent parsers live under
  `crates/adapters/<agent>/`.
- Never hand-edit canonical JSON or generated HTML. Rebuild them from sources.

Run `python3 -m tools.groundtruth --help` for the command surface and
`python3 -m tools.groundtruth check` after every change.

## 1. Mock-model dialect

Implemented dialects are in `tools/groundtruth/mock_model.py`. Add a missing
dialect there, keeping these invariants:

- Deterministic script selected from request shape: tool result closes the
  loop; `[use-tool]` requests one declared shell-like tool; otherwise return a
  fixed greeting.
- Support the dialect's real streaming sequence and non-streaming JSON.
- Fixed usage numbers: input 128, output 32.
- Log requests when configured; unknown routes return a logged 404.
- Add pure unit tests under `tools/groundtruth/tests/`. Tests must not require a
  listening socket so they run in restricted CI sandboxes.

Implemented dialects also include OpenAI Chat Completions and Gemini
`generateContent` (Antigravity CLI `modelProvider=gemini`).

## 2. Farm runner

Extend `tools/groundtruth/farm.py` and its `SCENARIOS` table. Preserve:

- Install the `watch.toml` surveyed/pinned release into an isolated work dir;
  never use a global agent install.
- Trust the binary's `--version` for the fixture directory name.
- Use an empty scenario HOME and the agent-specific home override.
- Point the agent at the local mock using a dummy credential only.
- Start with `basic-chat`, `tool-use`, and `resume`.
- Use the fixed cwd `/tmp/rosetta-farm/<scenario>/project`; cwd often leaks into
  persisted identifiers.
- Remove credentials, volatile staging, vendor bundles, and derived backups
  before harvest. Record each exclusion in layout/watch notes.
- Redact scenario/work paths and path-anchored usernames before committing.

Run a full networked capture with:

```sh
python3 -m tools.groundtruth farm <agent>
```

Re-infer from already committed fixtures without installation/network access:

```sh
python3 -m tools.groundtruth farm <agent> --schemas-only
```

Verify that transcripts contain the mock greeting, tool call/result/closing
text, and both resume turns. Search the harvested tree for usernames and local
absolute paths before accepting it.

## 3. Layout and facet extraction

Add `groundtruth/<agent>/layout.toml` using `format = "rosetta-layout/1"`.
Each stream needs `id`, `glob`, `file_format`, `discipline`, `evidence`, and
useful notes. Promote a stream to `fixture` only when a committed fixture file
matches it.

Nested unions keyed by tool name/subtype belong in
`tools/groundtruth/facets.py`. Facet extraction operates on raw JSON so it can
describe evidence before the typed parser supports it.

## 4. Raw Rust parser

Implement primary raw streams under `crates/adapters/<agent>/src/raw/`, using
only `groundtruth/<agent>/groundtruth.json` as the schema specification.

- Every raw struct flattens unknown fields into `Unknown` and implements
  `Drift`; `deny_unknown_fields` is forbidden.
- Unknown top-level discriminants are soft drift; malformed known variants are
  hard parse failures.
- Use `Option` only where canonical evidence proves absence/null.
- Preserve explicit null versus absent fields so lossless round-trip tests pass.
- Keep tool-result-like payloads as `serde_json::Value`; do not narrow by tool.
- Add no field without evidence.

Shared parser/test primitives are in `crates/adapter-support/`.

## 5. Replay tests and finish

Copy the existing five-test pattern into
`crates/adapters/<agent>/tests/fixtures_replay.rs`: corpus exists, every line
parses without drift, lossless round trip, all harvested files match the
canonical layout, and every fixture-backed stream has coverage.

Final gates:

```sh
python3 -m unittest discover -s tools/groundtruth/tests
python3 -m tools.groundtruth check
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

## Tier C/D agents

Do not fake hermetic evidence for OAuth/account-only or GUI-only agents. Follow
the researched manual capture protocol, mark claims `research` or `observed`,
and surface required credential/manual steps instead of improvising.

## Standing traps

- npm may block native-package postinstall scripts.
- SQLite evidence needs the main DB plus live `-wal`/`-shm` siblings.
- Structure census must never retain content, credentials, dynamic path keys,
  or identity-like enum values.
- Agents garbage-collect and rewrite data; harvest immediately after runs.
- Never parse credential streams merely because the layout inventories them.
