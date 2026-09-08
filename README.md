# rosetta

[![CI](https://github.com/vibefield/rosetta/actions/workflows/ci.yml/badge.svg)](https://github.com/vibefield/rosetta/actions/workflows/ci.yml)

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
3. **Services** — the planned localhost REST + WebSocket layer.

Current implementation status: all nine registered agents have ground-truth packs and
parser crates; the backfill pipeline and SQLite projection currently normalize Claude
Code and Codex transcript streams. Rewrites are generation-staged and replayable from a
normalized source-op journal inside the projection. The durable append-only raw ledger,
filesystem watcher/live lane, REST/WebSocket server, and generated clients have not
shipped yet. Until the raw-ledger rebuild path exists, a projection schema mismatch
fails closed instead of deleting the database.

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

`farm` without `--schemas-only` installs and executes downloaded agent CLIs. It fails
closed unless run inside a container/VM (prefer a runner image pinned by SHA-256
digest), explicitly attested with `ROSETTA_FARM_ISOLATED=1`, or deliberately overridden
with `--allow-host-execution` on a disposable research machine.

## Development and delivery

The canonical repository is the private [`vibefield/rosetta`](https://github.com/vibefield/rosetta).
Use a feature branch and pull request, then squash merge after **CI passed** succeeds.
CI runs on pull requests and `main`, and can be started manually. It checks workflow
syntax, the committed ground-truth artifacts, Python tests, Rust formatting, Clippy
with warnings denied, and workspace tests on Linux and Apple Silicon macOS.
Rust comes from `rust-toolchain.toml`; builds use the committed `Cargo.lock` with
`--locked`. The evidence tools require Python 3.12 or newer; CI uses Python 3.13.
External actions are pinned to commit SHAs. Workflow tokens default to read-only;
only the draft-release job receives contents-write permission. Dependabot proposes
weekly grouped Rust and GitHub Actions updates. Review and merge those through CI.

The **Release** workflow builds native `rosettad` archives for Linux x64/ARM64 and
macOS Intel/Apple Silicon. Linux archives target Ubuntu 24.04's glibc environment;
macOS archives are unsigned command-line binaries, not notarized desktop installers.
Each archive contains the binary, README, and `BUILD.json` with version, target,
source commit, and compiler. Builds run a discovery smoke test against an empty
home directory. `SHA256SUMS` verifies the four archives.

To exercise delivery without publishing a version:

```sh
gh workflow run release.yml --repo vibefield/rosetta --ref main
```

Download the `release-bundle` artifact from that run within seven days. For a release,
merge the version update in `[workspace.package]` in `Cargo.toml` and the resulting
`Cargo.lock` changes, then tag that exact commit on `main`:

```sh
git switch main
git pull --ff-only
# Replace 0.1.0 below with the version committed in Cargo.toml.
git tag -a v0.1.0 -m 'Release v0.1.0'
git push origin v0.1.0
```

The workflow rejects mismatched versions and commits outside `main`, reruns CI,
builds all four archives, verifies their checksums, then creates a **draft** GitHub
Release with generated notes. Review its assets and publish the draft in GitHub.
Rerunning publication for an existing release fails rather than replacing assets;
if a draft needs correction, delete only that unpublished draft before rerunning.
Manual workflow runs never publish, even when dispatched against a tag. No cloud
account, deployment secrets, or automatic desktop updater is configured here.

Private-repository branch protection requires GitHub Team for the organization.
On the current Free plan, **CI passed is visible but not enforced**: users with write
access can still push or merge directly. After upgrading, apply the prepared policy:

```sh
gh api --method PUT repos/vibefield/rosetta/branches/main/protection \
  --input .github/main-protection.json
```

That policy requires an up-to-date passing CI run and a pull request, blocks force
pushes/deletion, requires resolved conversations, and applies to administrators.
It allows zero approving reviews so a sole maintainer can merge their own PRs.
