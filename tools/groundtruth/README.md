# Ground-truth tools

This standard-library Python package owns the development-time evidence
pipeline. It deliberately lives outside both `groundtruth/` (data only) and
`crates/` (production/runtime Rust). It requires Python 3.11 or newer.

Run commands from the repository root:

```sh
python3 -m tools.groundtruth check
python3 -m tools.groundtruth farm claude-code --schemas-only
python3 -m tools.groundtruth farm codex --schemas-only
python3 -m tools.groundtruth render groundtruth/codex/groundtruth.json
python3 -m tools.groundtruth mock-model 8080
python3 -m tools.groundtruth census claude-code \
  groundtruth/claude-code/sources/observed-YYYY-MM-DD.json \
  --label observed-YYYY-MM-DD
```

`farm` performs networked package installation unless `--schemas-only` is
used. Every successful schema rebuild writes both `groundtruth.json` and its
sibling `groundtruth.html`.
