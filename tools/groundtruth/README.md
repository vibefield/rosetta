# Ground-truth tools

This standard-library Python package owns the development-time evidence
pipeline. It deliberately lives outside both `groundtruth/` (data only) and
`crates/` (production/runtime Rust). It requires Python 3.11 or newer.

Run commands from the repository root:

```sh
python3 -m tools.groundtruth check
python3 -m tools.groundtruth farm claude-code --schemas-only
python3 -m tools.groundtruth farm codex --schemas-only
python3 -m tools.groundtruth farm kimi-code --schemas-only
python3 -m tools.groundtruth farm opencode --schemas-only
python3 -m tools.groundtruth farm pi --schemas-only
python3 -m tools.groundtruth farm grok --schemas-only
python3 -m tools.groundtruth farm antigravity --schemas-only
python3 -m tools.groundtruth farm copilot --schemas-only
python3 -m tools.groundtruth farm hermes --schemas-only
python3 -m tools.groundtruth render groundtruth/codex/groundtruth.json
python3 -m tools.groundtruth mock-model 8080
python3 -m tools.groundtruth census claude-code \
  groundtruth/claude-code/sources/observed-YYYY-MM-DD.json \
  --label observed-YYYY-MM-DD --writer-version 2.1.241
python3 -m tools.groundtruth census kimi-code \
  groundtruth/kimi-code/sources/observed-YYYY-MM-DD.json \
  --label observed-YYYY-MM-DD
python3 -m tools.groundtruth census grok \
  groundtruth/grok/sources/observed-YYYY-MM-DD.json \
  --label observed-YYYY-MM-DD
```

`farm` performs networked package installation unless `--schemas-only` is
used. Every successful schema rebuild writes both `groundtruth.json` and its
sibling `groundtruth.html`.

Organic censuses retain real protocol enum values and built-in tool names, but
remove literal IDs, paths, filenames, prompts, titles, arbitrary map keys, and
configured MCP names. Claude transcript censuses must pass the pinned
`--writer-version`; unversioned sidecar streams are still included.

Every `watch.toml` declares `groundtruth.status = "captured"` or `"blocked"`, and
captured packs record any remaining evidence limitations in `known_gaps`.
Normal checks report blocked packs without silently treating them as complete;
`python3 -m tools.groundtruth check --require-complete` fails on any blocked pack.
Use `check --fix` to refresh canonical JSON, HTML, census policy migrations, and
exact fixture manifests.
