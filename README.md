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
the final handoff fsyncs and promotes the named staged ledger first, then atomically installs the validated
SQLite file. If the process exits first, a durable generation manifest retains enrollment and root
binding; the next launch replays sealed ledger evidence into fresh RAM, fences any unsealed source
generation, and finishes from the current source tree. Project/session/timeline reads remain progressive, while global heatmap, usage, and
contribution queries return `not_ready` until canonical publication; detached search starts after
that gate. Finite `ingest` and `rebuild` retain the same private-build/atomic-publication contract.
The app starts plain `serve`: a complete projection is immediately live, arms watchers before its
bounded current-inventory skim, and performs one forced cursor-relative catch-up without presenting
another cold/backfill state. `--restart-incomplete` remains an explicit legacy archive/restart tool;
it is not the app's normal recovery path.
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

The [API documentation website](docs/site/README.md) covers onboarding, every
HTTP/WebSocket route, the TypeScript client, and generated wire types. Run
`npm --prefix docs/site run dev` to open it at `http://127.0.0.1:5190` (Node 22+;
no dependency installation required). Build static output with
`npm --prefix docs/site run build`.

Proposed lifecycle contracts are documented separately: [daemon ownership and
recovery](docs/DAEMON_LIFECYCLE_PROPOSAL.md), [cold/live ingestion
controls](docs/INGESTION_API_PROPOSAL.md), and the [desktop/Vibefield integration
review](docs/INGESTION_API_REVIEW.md). These additions are not implemented routes.

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

## Development and delivery

The canonical repository is the private [`vibefield/rosetta`](https://github.com/vibefield/rosetta).
Use a feature branch and pull request, then squash merge after **CI passed** succeeds.
CI runs on pull requests and `main`, and can be started manually. It checks workflow
syntax, the committed ground-truth artifacts, Python tests, Rust formatting, Clippy
with warnings denied, and workspace tests on Linux and Apple Silicon macOS. The npm
job builds both libraries and the app, runs the SDK and complete app unit suites,
checks documentation, tests the packed libraries in a clean consumer, and exercises
the room host in Electron.
Rust comes from `rust-toolchain.toml`; builds use the committed `Cargo.lock` with
`--locked`. The evidence tools require Python 3.12 or newer; CI uses Python 3.13.
External actions are pinned to commit SHAs. Workflow tokens default to read-only;
only the draft-release job receives contents-write permission, and only the npm
publish job receives an OIDC identity. Dependabot proposes weekly grouped Rust,
npm, and GitHub Actions updates. Review and merge those through CI.

### npm packages

The public packages are `@vibefield/rosetta-protocol` (SDK and wire types) and
`@vibefield/rosetta-room` (React room, fixtures, and styles). Both ship compiled ESM
and declarations. React 19 is supplied by the room's host. Package versions move
together; the wire protocol version remains independent. Publishing does not grant
an open-source license: the manifests retain `UNLICENSED` until a license is chosen.

Use Node 24 and npm 12.0.1 for development and releases:

```sh
npm ci
npm run build
npm test
npm run check:packages
```

One root lockfile covers the protocol, room, and private app. Mille dependencies
come from npm; no sibling checkout is required. `check:packages` builds tarballs in
`dist/npm`, installs them in a temporary consumer, tests ESM and shared grant vectors,
checks NodeNext declarations and the DOM-free version export, then bundles the room
and its CSS. The tarballs include only `dist`, package metadata, and the README.

The first publish uses an authenticated maintainer, after the checks above:

```sh
node tools/npm/publish.mjs
npm trust github @vibefield/rosetta-protocol --repo vibefield/rosetta --file npm-release.yml --allow-publish --yes
npm trust github @vibefield/rosetta-room --repo vibefield/rosetta --file npm-release.yml --allow-publish --yes
```

Trust setup may require account 2FA. Future releases use GitHub-hosted OIDC without
an npm token secret. The source repository is private, so npm provenance is disabled.
The **npm release** workflow publishes only on `npm-v*` tag pushes; a manual run is
a complete dry run. It checks main ancestry, stable tag/version agreement, all CI,
and the exact tarballs before publishing protocol first and room second. Reruns skip
only versions whose registry integrity exactly matches the verified artifact.

For the next version, update both library manifests and the protocol dependency in
the room, refresh the root lockfile, and merge through **CI passed**. Tag that main
commit, using its version (for example):

```sh
git tag -a npm-v0.1.1 -m 'Rosetta npm 0.1.1'
git push origin npm-v0.1.1
```

Use `gh workflow run npm-release.yml --ref main` to verify a release without publishing.

### Daemon archives

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
