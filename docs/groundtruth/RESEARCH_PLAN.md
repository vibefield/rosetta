# Ground Truth Research Plan

How rosetta obtains, tracks, and automates the ground-truth data schemas of every
mainstream coding agent — reliably, per release, with zero dependence on any
developer's machine.

## 0. Reliability requirements

This is a product. The ground-truth pipeline must be:

1. **Machine-independent** — reproducible by any contributor or CI runner that has
   never run any of these agents. A developer's local agent dataset is never
   load-bearing; at most it corroborates.
2. **Release-tracked** — every agent release that can change an on-disk schema is
   detected automatically, and the schema delta is captured as a reviewable diff in
   this repo.
3. **Fixture-automated** — schema claims are backed by committed fixture corpora that
   CI replays; "we believe the format is X" is never the end state, "the parser passes
   the v2.1.3 corpus" is.

## 1. The four ground-truth channels (ranked by reliability)

| # | Channel | What it gives | When usable |
|---|---------|---------------|-------------|
| 1 | **Writer source code** — the agent's own persistence/serialization code at a pinned release tag | The schema, exactly, including optionality and enum ranges the data may never show | OSS agents; closed agents shipping readable JS in npm tarballs |
| 2 | **Hermetic fixture generation** — run the pinned agent binary headlessly against a mock model server, harvest its data dir | Real bytes the agent actually writes; the behavioral truth source code can obscure | Any agent with a headless mode; fully hermetic when a base-URL override exists |
| 3 | **Official docs / changelogs / release notes** | Intent, version boundaries, deprecation warnings | All agents; never sufficient alone |
| 4 | **Observed local/community data** | Corroboration, edge cases from long organic histories | Opportunistic only — never load-bearing |

Channels 1 and 2 are the load-bearing pair: source tells you what *can* be written,
fixtures prove what *is* written. Divergence between them is itself a finding.

## 2. Fixture farm

A repo-owned harness (`python3 -m tools.groundtruth farm`) that, given
`(agent, version, scenario)`, produces a committed fixture corpus.

**Runner.** Containerized Linux runner by default (agents are installed fresh from the
authoritative channel at a pinned version into an empty `$HOME`); a macOS lane for
agents whose paths/behavior differ. The empty-home property is the point: the harvested
data dir contains *only* what the scenario produced.

**Mock model server.** One small standard-library Python server
(`tools/groundtruth/mock_model.py`) returning scripted responses per scenario
(including canned tool-call requests, streaming deltas, usage
blocks). The survey shows it needs **four wire dialects**: OpenAI Chat Completions
(OpenCode, Kimi, Pi, Hermes), **OpenAI Responses** (Codex accepts only
`wire_api = "responses"`), **Anthropic Messages** (Claude Code, Pi), and **Gemini
`generateContent`** (Gemini CLI). Agents pointed at it via their base-URL override run
with **no credentials, no cost, and deterministic outputs** — this is what makes
fixture generation CI-able.

**Auth tiers.** Each agent is classified by how hermetic its runs can be:

- **Tier A** — OSS + endpoint override → fully hermetic, run on every release.
- **Tier B** — closed-source but headless CLI + endpoint override → hermetic, every release.
- **Tier C** — headless but hard auth wall (OAuth/subscription only) → scheduled runs
  with dedicated low-cost test accounts, secrets held in CI; cadence per release but
  human-triggered.
- **Tier D** — no scriptable surface (GUI/IDE-only) → a written **manual capture
  protocol**: a versioned checklist of in-app actions + a harvest script; slowest lane,
  still producing committed, versioned fixtures.

**Scenario suite.** A capability matrix, instantiated per agent (skipping unsupported
capabilities):

`basic-chat`, `multi-turn`, `tool-use-read/write/bash`, `subagent-spawn`,
`todo/plan-artifacts`, `mcp-tool`, `compaction/summarization`, `session-resume`,
`error/abort`, `parallel-sessions`.

**Harvest → fixture.** Each headless run yields two surfaces: the **persisted data
dir** (the ground truth rosetta ingests) and the **stdout JSON/JSONL event stream**
most CLIs emit in headless mode — capture both; the stream corroborates the files.
After each scenario: snapshot the agent home dir → **redact**
(usernames, keys, work directories, path-anchored home directories) → traverse in
stable file order → write to the agent data pack's
`groundtruth/<agent>/fixtures/<agent-version>/<scenario>/`. Timestamps and UUIDs
remain as evidence, so fixtures are structurally deterministic but not byte-for-byte
reproducible. A manifest records agent version, scenario, generator, mock dialect,
and file inventory with byte sizes.

**Schema snapshot.** A shape inferencer runs over every harvested stream and emits a
**unified ground-truth document** (`rosetta-groundtruth/1` — layout + record types +
field shapes with requiredness and counts) into the agent's
`groundtruth/<agent>/sources/` dir. Organic-data censuses and SQLite schema dumps land
there too, and OSS writer-source exports will as well. Those are intermediates:
`build_canonical` merges them (evidence-ranked per record type) into
**`groundtruth/<agent>/groundtruth.json` — the ONE canonical file per agent that
solely and fully reflects its ground truth**. The pipeline also commits the sibling
`groundtruth.html` visualization; the JSON diff across a release regeneration **is**
the drift report, reviewed like any code change.

## 3. Release watch

Each agent data directory carries a `watch.toml`:

```toml
[release]
channel  = "npm"                  # npm | github-releases | brew | pypi | manual
package  = "@anthropic-ai/claude-code"
pinned   = "2.1.34"               # version the committed fixtures/schemas reflect

[source]                          # OSS agents only
repo     = "https://github.com/…"
persistence_paths = ["src/session/…"]
```

A scheduled CI job polls every channel (npm dist-tags, GitHub releases Atom feeds,
brew/pypi APIs). On a new version it runs the fixture farm for Tier A/B agents and
opens a PR containing: bumped pin, regenerated fixtures, regenerated schema snapshots,
and the drift report. Green + empty drift merges trivially; non-empty drift is exactly
the review that keeps rosetta current with agent releases. Tier C/D agents get an issue
filed with the capture protocol linked instead of an automatic run.

## 4. Per-agent questionnaire

Every agent's plan below answers the same nine questions: identity & cadence;
authoritative release channel; open-source status & what's inspectable; on-disk data
footprint (macOS); headless mode; endpoint override / auth wall; version detection;
source pointers to persistence code; pipeline risks. An agent is **survey-complete**
when all nine are answered with citations and its Tier is assigned.

## 5. Per-agent plans

Survey complete (2026-08-24). Roster at a glance:

| Agent | Tier | Watch channel | Primary on-disk artifact |
|---|---|---|---|
| Claude Code | **B** | npm dist-tags `@anthropic-ai/claude-code` | `~/.claude/projects/<slug>/<id>.jsonl` |
| Codex CLI | **A** | GitHub `openai/codex/releases.atom` | `~/.codex/sessions/…/rollout-*.jsonl` (SQLite migration watch) |
| Gemini CLI | **A** | npm dist-tags `@google/gemini-cli` | `~/.gemini/tmp/<hash>/chats/` (JSON→JSONL migration) |
| Grok Build | **A** | `https://x.ai/cli/stable` (plain-text version) | `~/.grok/sessions/<cwd>/<uuid>/` (multi-file) |
| Antigravity | **C/D** | CLI: Cloud Run manifest JSON; IDE: changelog scrape | `~/.gemini/antigravity-ide/conversations/` (`.pb`→SQLite) |
| Copilot CLI | **B** | npm `@github/copilot` | `~/.copilot/session-state/<id>/events.jsonl` |
| OpenCode | **A** | npm `opencode-ai` | `~/.local/share/opencode/opencode.db` (+legacy JSON) |
| Kimi Code | **A** | npm `@moonshot-ai/kimi-code` | `~/.kimi-code/sessions/…/agents/*/wire.jsonl` |
| Cursor CLI | **C** | changelog scrape (no artifact registry) | `~/.cursor/chats/…/store.db` + transcript JSONL |
| Hermes | **A** | GitHub `NousResearch/hermes-agent` releases | `~/.hermes/state.db` (SQLite, versioned schema) |
| Pi | **A** | npm `@earendil-works/pi-coding-agent` | `~/.pi/agent/sessions/…/*.jsonl` (versioned header) |

Nine of eleven are hermetically fixture-able (Tier A/B). Only Cursor CLI (no endpoint
override) and Antigravity (OAuth-only) need credentialed or manual lanes.

### 5.1 Hermes (Nous Research Hermes Agent) — **Tier A**

Nous Research's self-improving personal/coding agent: one Python agent core behind a
CLI/TUI, an Electron app, and a messaging gateway. MIT, fully OSS
([repo](https://github.com/NousResearch/hermes-agent)). Releases multiple times per
week, dual-tagged semver (`v0.20.5`) + calver (`v2026.8.19`).

- **Track:** GitHub Releases API on `NousResearch/hermes-agent` (changelog = release
  notes; no CHANGELOG.md). Normalize the dual tag scheme to semver in `watch.toml`.
- **Install for farm:** no npm/brew — curl install script or, preferably, the
  **per-release Docker images** (cleanest pinned-version substrate for CI).
- **On-disk ground truth (macOS):** everything under `~/.hermes/` (`HERMES_HOME`
  override — useful for harvest isolation). Config `config.yaml`, credentials `.env`,
  and **sessions in SQLite: `~/.hermes/state.db`**, schema tracked by an in-DB
  `schema_version` table (currently **v23**): `sessions` (model, parent_session_id,
  token counts incl. cache_read/cache_write/reasoning, billing), `messages` (role,
  content, `tool_calls` JSON, `api_content` byte-fidelity sidecar, compacted/active
  flags), `session_model_usage`, three FTS5 tables, `gateway_routing`,
  `async_delegations`. Legacy JSONL in `~/.hermes/sessions/` is dead.
- **Source pointers:** `hermes_state.py` (`SessionDB`; WAL, `_reconcile_columns()`
  additive migrations, `export_session()`/`export_all()`); docs mirrored in-repo at
  `website/docs/developer-guide/session-storage.md`.
- **Fixtures:** headless is first-class — `hermes -z "prompt"`, `hermes chat -q`,
  `--query-file -`, `--usage-file report.json`, `-Q`, and `--ignore-user-config` /
  `--safe-mode` for hermetic runs. Custom endpoints supported ("your own endpoint",
  OpenAI-compatible) via the `hermes model` wizard / `config.yaml` — exact provider
  block shape UNVERIFIED: run the wizard once in the farm and capture the generated
  config as the template. Because the store is binary SQLite, commit BOTH the `.db`
  harvest AND two diffable derivatives: a full SQL schema dump (`sqlite3 .schema` +
  `schema_version`) and `export_session()` output (format UNVERIFIED — pin down in
  first farm run).
- **Risks:** very fast release cadence + actively evolving schema (v23 already) →
  expect frequent drift PRs; diff schema dump per release. Dual tags need
  normalization. Curl install awkward to pin — prefer Docker lane.

### 5.2 Pi (Earendil Works / Mario Zechner) — **Tier A**

Minimal, extensible terminal coding agent (Node.js CLI/TUI + SDK) by Mario Zechner,
now published under the **Earendil Works** org. MIT, OSS
([repo](https://github.com/earendil-works/pi), transferred from `badlogic/pi-mono` —
old URL redirects). Current `v0.84.3`; weekly-to-biweekly cadence.

- **Track:** npm registry, package **`@earendil-works/pi-coding-agent`** (the old
  `@mariozechner/pi-coding-agent` is deprecated at v0.73.1 — do not track it, but
  keep it available for historical fixtures). GitHub Releases mirror versions with
  detailed notes.
- **Install for farm:** `npm install -g --ignore-scripts @earendil-works/pi-coding-agent@<pin>`.
- **On-disk ground truth (macOS):** sessions at
  `~/.pi/agent/sessions/--<cwd-hyphenated>--/<timestamp>_<uuid>.jsonl` — **JSONL with
  an explicitly versioned `SessionHeader` first line** (`version`: 1 linear → 2 tree
  → 3 current; auto-migrates on load; optional `parentSession` for forks). Entries
  extend `SessionEntryBase` (`type`, 8-hex `id`, `parentId`, ISO `timestamp`);
  message roles include user/assistant/toolResult/bashExecution/custom/
  branchSummary/compactionSummary; structural types include `model_change`,
  `thinking_level_change`, `compaction` (`retainedTail`), `label`, `session_info`.
  Config: `~/.pi/agent/settings.json` + project `.pi/settings.json`, `trust.json`,
  `models.json`, `auth.json` (0600). `PI_CODING_AGENT_SESSION_DIR` overrides the
  session dir — use it to harvest into a clean directory.
- **Source pointers:** `packages/coding-agent/src/core/session-manager.ts`,
  `.../core/messages.ts`, `packages/agent/src/types.ts`, `packages/ai/src/types.ts`;
  canonical format doc `packages/coding-agent/docs/session-format.md` (a rare gift:
  the vendor documents the session schema — diff this file per release too).
- **Fixtures:** `pi -p "prompt"` (stdin piping works), `--mode json` for event-stream
  capture; `PI_OFFLINE=1` + `PI_SKIP_VERSION_CHECK=1` for hermetic CI. Mock endpoint
  is first-class: custom providers in `models.json` speaking OpenAI
  Completions/Responses, Anthropic Messages, or Google GenAI protocols (Ollama/vLLM
  explicitly supported) — point at the farm's mockmodel, no credentials needed.
- **Risks:** pre-1.0 — breaking format bumps are fair game (the v3 rename just
  happened); the org/scope migration could recur (watch deprecation notices); the
  cwd-encoded session-dir naming is an implementation detail that may change.

### 5.3 OpenCode (Anomaly Co, ex-SST) — **Tier A**

Terminal agent (TUI + CLI + client/server), MIT, OSS. Originally SST, now developed
by **Anomaly Co** ([repo](https://github.com/anomalyco/opencode) — `sst/opencode`
redirects). Near-daily releases (current `v1.18.22`).

- **Track:** npm registry `opencode-ai` `latest` dist-tag (GitHub `releases.atom`
  equivalent and carries changelog notes).
- **Install for farm:** `npm i -g opencode-ai@<pin>` (wrapper + platform binary deps).
- **On-disk ground truth (macOS):** root `~/.local/share/opencode/` — `auth.json`,
  `log/`, and per-project storage under `project/<slug>/storage/` (git repos) or
  `project/global/storage/`. **Storage recently migrated (~v1.2+) from per-record
  JSON (`storage/{session,message,part}/*.json`) to one SQLite DB
  `~/.local/share/opencode/opencode.db`** — `OPENCODE_DB` overrides the path (harvest
  isolation), `opencode db path` prints it. Both formats exist in the wild; the
  adapter must read both. Config: `~/.config/opencode/opencode.jsonc` + per-project
  `opencode.json`.
- **Source pointers:** `packages/opencode/src/storage/storage.ts` and
  `storage/schema.ts` (branch `dev`); session domain under
  `packages/opencode/src/session/`.
- **Fixtures:** `opencode run "prompt" -m mock/mock-model --format json --auto`
  (json = raw event stream; `--auto` auto-approves; `-s`/`-c` resume; `--attach` to a
  running `opencode serve`). Endpoint override is first-class: declare a provider
  with `"npm": "@ai-sdk/openai-compatible"` and a local `baseURL` in `opencode.json`,
  no real credentials.
- **Risks:** near-daily releases → the release-watch will batch (e.g. weekly roll-ups)
  rather than PR-per-release; the JSON→SQLite migration is fresh with known bugs
  (issues #13654, #16885) and dual on-disk formats; the SQLite schema is internal,
  changeable without release-note callouts — schema-dump diffing is the only reliable
  drift signal; possible further org/URL churn.

### 5.4 Kimi Code CLI (Moonshot AI) — **Tier A**

Moonshot's coding CLI — TypeScript successor to the Python `kimi-cli` (legacy being
wound down; `kimi migrate` imports old data). MIT, OSS
([repo](https://github.com/MoonshotAI/kimi-code)). Pre-1.0 (`0.38.0`), fast-moving.

- **Track:** npm registry `@moonshot-ai/kimi-code` (bin `kimi`). Changelog on
  kimi.com/code docs. Legacy PyPI `kimi-cli` only if historical coverage is wanted.
- **On-disk ground truth (macOS):** root **`~/.kimi-code/`** (`KIMI_CODE_HOME`
  override — harvest isolation): `config.toml`, `tui.toml`, `mcp.json`; **sessions at
  `sessions/<workDirKey>/<sessionId>/` where `workDirKey = wd_<slug>_<sha256-12>`**;
  per session `state.json` (metadata), **`agents/main/wire.jsonl`** (complete wire
  event record; sub-agents as `agents/agent-N/wire.jsonl`), `upcoming-goals.json`,
  `tasks/`, `cron/`; also `credentials/` (0600), per-session and global logs,
  `user-history/<md5(workdir)>.jsonl`. Legacy `~/.kimi/` layout differs
  (`context.jsonl` + `wire.jsonl`).
- **Source pointers:** exact persistence module UNVERIFIED — grep for the
  `wire.jsonl`/`state.json` writers under `src/`; the official data-locations doc is
  the normative schema description (diff it per release).
- **Fixtures:** `kimi -p "prompt"` (non-interactive; **implicitly auto-approves
  tools** — sandbox the runner), `--output-format stream-json`, `--session`/`-c`
  resume. Mock endpoint: **zero-config env synthesis — `KIMI_MODEL_NAME` +
  `KIMI_MODEL_API_KEY` + `KIMI_MODEL_BASE_URL` + `KIMI_MODEL_PROVIDER_TYPE=openai|anthropic|kimi`**
  creates a temporary provider, ideal for CI (note: regular provider keys are NOT
  read from env — they must be in `config.toml`; the `KIMI_MODEL_*` synth is the
  exception).
- **Risks:** pre-1.0 wire schema churn without semver signaling; mid-migration
  product with two data dirs (`~/.kimi` vs `~/.kimi-code`); three doc mirrors that
  disagree — treat kimi.com/code as canonical; headless auto-approval demands
  container isolation; OAuth vs API-key paths behave differently.

### 5.5 Cursor CLI (Anysphere) — **Tier C**

Cursor's terminal agent. **Closed source**; binary renamed `cursor-agent` → `agent`
(Jan 2026; alias kept). Date-based versioning, roughly weekly-to-biweekly.

- **Track:** no npm/brew/GitHub artifact channel. UNVERIFIED best option: scrape
  `cursor.com/docs/cli/changelog` (+ probe the `cursor.com/install` endpoint for
  version metadata). This is the weakest watch in the roster — accept it.
- **On-disk ground truth (macOS):** versioned installs under
  `~/.local/share/cursor-agent/`; session data under `~/.cursor/`: **SQLite metadata
  `~/.cursor/chats/<workspace-hash>/<session-id>/store.db`** (WAL — harvest must
  checkpoint or copy `-wal`/`-shm` siblings) and **JSONL transcripts
  `~/.cursor/projects/<project>/agent-transcripts/<session-id>/<session-id>.jsonl`**
  (write-only logs). `store.db` internals are undocumented (community
  reverse-engineering only). Distinct from the Cursor IDE's `state.vscdb` — do not
  conflate.
- **Fixtures:** headless exists — `agent -p "prompt" --output-format json`
  (`--force` for edits, `--model`, `--resume`) — **but there is no custom-endpoint
  mechanism**: all traffic proxies through Cursor's backend under a Cursor account
  (`agent login` or `CURSOR_API_KEY`). Farm runs therefore need a funded test
  account, hit real models (cost, rate limits, nondeterminism), and run on a
  schedule/manual trigger rather than per-release.
- **Risks:** closed source + undocumented store.db = reverse-engineered knowledge
  decays every release; no confirmed way to install a pinned old version (fixtures
  are capture-when-current, irreplaceable later — archive aggressively); date-based
  versioning and the binary rename complicate version parsing.

### 5.6 Claude Code (Anthropic) — **Tier B**

The flagship source (and rosetta's primary consumer target). Terminal CLI + desktop +
web + IDE extensions; **closed source** (npm license "SEE LICENSE IN README.md"; the
GitHub repo is issues + changelog only). Near-daily releases; npm dist-tags `stable` /
`latest` / `next`.

- **Track:** npm `@anthropic-ai/claude-code` dist-tags
  (`npm view @anthropic-ai/claude-code dist-tags`); changelog at
  `anthropics/claude-code/CHANGELOG.md`. Fixture `stable` + `latest`, not every patch.
- **On-disk ground truth (macOS):** transcripts as **JSONL at
  `~/.claude/projects/<cwd-slug>/<session-id>.jsonl`** (slug = cwd with
  non-alphanumerics → `-`; long paths truncated + hashed). **Default 30-day retention
  (`cleanupPeriodDays`)** — the ledger argument in ARCHITECTURE.md §3.5, confirmed
  official. `CLAUDE_CONFIG_DIR` relocates the whole tree — perfect harvest isolation.
  Line schema (community-documented; officially internal): types
  `user`/`assistant`/`system`/`summary`/`file-history-snapshot`; common fields
  `uuid`, `parentUuid`, `sessionId`, `timestamp`, `cwd`, `gitBranch`, `isSidechain`,
  and **a per-line `version` stamp of the writing CLI** — rosetta's per-record
  version detection, confirmed. Assistant lines add `requestId`,
  `message.{model,content,stop_reason,usage}`; tool results in `toolUseResult`.
  Subagents as `agent-<id>.jsonl`; prompt history `~/.claude/history.jsonl`;
  settings global + project + `.local`. Anthropic's docs state the entry format "is
  internal to Claude Code and changes between versions" — schema tracking here is
  load-bearing, not paranoia.
- **Source pointers:** none (compiled/bundled npm tarball; static inspection weak).
  Ground truth = fixtures + tarball diffing only.
- **Fixtures:** `claude -p "prompt" --output-format stream-json` with **`--bare`**
  (skips hooks/plugins/CLAUDE.md, never touches the OAuth keychain) +
  `ANTHROPIC_BASE_URL` → mockmodel (Anthropic Messages dialect) + dummy
  `ANTHROPIC_API_KEY` (always used under `-p`) + `CLAUDE_CONFIG_DIR` → clean harvest
  dir. Never pass `--no-session-persistence` (it suppresses the very files we want).
- **Risks:** schema explicitly unstable at patch frequency — the drift pipeline earns
  its keep here; some record types (`summary`, `file-history-snapshot`) only appear
  under interactive behaviors, so the scenario suite needs resume/compaction flows to
  exercise them; closed source means fixtures are the *only* reliable signal.

### 5.7 Codex CLI (OpenAI) — **Tier A**

OpenAI's terminal agent. **Apache-2.0, fully OSS** — Rust implementation under
`codex-rs/` ([repo](https://github.com/openai/codex)). Multiple releases/week plus
rolling alphas; track `latest` only.

- **Track:** GitHub Releases (`openai/codex/releases.atom`) — authoritative source of
  binaries and release notes; npm `@openai/codex` secondary (JS shim over
  per-platform native binaries).
- **On-disk ground truth (macOS):** `$CODEX_HOME` (default `~/.codex` — override for
  harvest isolation). Sessions: **JSONL "rollouts" at
  `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl`** — complete event
  stream: prompts, responses, tool calls/results, approval decisions, token usage.
  Config `config.toml`; prompt history `history.jsonl` (`history.persistence`,
  `history.max_bytes`). **Churn signal: a JSONL→SQLite migration appears underway**
  (config reference now describes session/history state in a SQLite DB rooted at
  `sqlite_home`, exact filename UNVERIFIED) — watch every release for it.
- **Source pointers:** rollout persistence in **`codex-rs/core/src/rollout/`**
  (`RolloutRecorder` in `recorder.rs`; may be extracted to its own crate — re-pin
  `persistence_paths` per release); serialized types (RolloutItem/EventMsg) in the
  protocol crate under `codex-rs/`.
- **Fixtures:** `codex exec "prompt" --json` (stdout = JSONL event stream). Mock via
  `[model_providers.mock]` in config.toml: `base_url` → mockmodel, `env_key` naming a
  dummy env var, **`wire_api = "responses"`** — the mock must speak the OpenAI
  Responses API, not chat-completions. No OAuth wall on this path.
- **Risks:** the SQLite migration could swap the ground-truth artifact entirely
  mid-pipeline; rollouts embed compaction history and can reach GBs — the scenario
  suite must include compaction; crate reorgs move the files `watch.toml` points at.

### 5.8 Gemini CLI (Google) — **Tier A**

Google's terminal agent. **Apache-2.0, fully OSS** — TypeScript monorepo
([repo](https://github.com/google-gemini/gemini-cli)). Fixed cadence: weekly `stable`
+ `preview`, daily `nightly` — npm dist-tags map exactly; fixture `latest` + `preview`.

- **Track:** npm `@google/gemini-cli` dist-tags (one poll covers all three channels);
  GitHub Releases mirror with release notes.
- **On-disk ground truth (macOS):** settings `~/.gemini/settings.json`; session
  recordings at **`~/.gemini/tmp/<project_hash>/chats/`** — historically single JSON
  files rewritten each turn by `ChatRecordingService`, **actively migrating to
  append-only JSONL** (dual formats in the wild). Records include messages, tool
  outputs, structured subagent data. Two sharp edges for capture: storage sits under
  `tmp/` with retention cleanup (harvest immediately after each run), and
  `<project_hash>` must be recomputed from core source to locate files.
- **Source pointers:** **`packages/core/src/services/chatRecordingService.ts`** +
  **`chatRecordingTypes.ts`** (the record type definitions — ideal for source-derived
  schema snapshots) + its test file.
- **Fixtures:** `gemini -p "prompt" --output-format stream-json` (events: `init`,
  `message`, `tool_use`, `tool_result`, `error`, `result`; distinct exit codes).
  Mock via **`GOOGLE_GEMINI_BASE_URL`** + dummy `GEMINI_API_KEY` speaking the Gemini
  `generateContent` dialect — **with a clean `~/.gemini`**: the override is reportedly
  ignored when a cached Google OAuth session exists, so the farm must force API-key
  auth.
- **Risks:** JSON→JSONL migration in flight (fixture both); recording internals have
  churned repeatedly; the rewrite-each-turn JSON legacy means tailing semantics
  differ from append-only agents — capture layer must support both file disciplines.

### 5.9 Grok Build (xAI) — **Tier A**

xAI's terminal agent (binary `grok`, Rust TUI + headless + ACP). Announced May 2026;
**Apache-2.0 with full Rust source** at
[xai-org/grok-build](https://github.com/xai-org/grok-build) (a mirror of their
internal monorepo — `SOURCE_REV` records the internal SHA). Do not confuse with the
unofficial `superagent-ai/grok-cli`. Current `1.0.5`, rapid cadence.

- **Track:** no npm/brew/GitHub Releases. Authoritative: **`https://x.ai/cli/stable`**
  (plain-text latest version); install script accepts a pinned `bash -s X.Y.Z`;
  artifacts from `x.ai/cli` with a GCS fallback bucket. Changelog at
  `x.ai/build/changelog`.
- **On-disk ground truth (macOS):** base `~/.grok` (**`GROK_HOME` override** —
  harvest isolation). Per session
  `~/.grok/sessions/<url-encoded-cwd>/<UUIDv7>/`: `summary.json` (documented fields
  incl. `parent_session_id`, `num_messages`, `current_model_id`), **`updates.jsonl`
  (ACP update stream — the authoritative conversation log)**, `chat_history.jsonl`
  (raw model messages), `plan.json`, `rewind_points.jsonl`, `signals.json`,
  `feedback.jsonl`, `compaction_checkpoints/`, `subagents/`. Long cwds fall back to
  slug+hash with a `.cwd` file. Also `config.toml`, `auth.json`, an FTS5 SQLite
  search index, `worktrees/`. The vendor documents the session layout in-repo
  (`crates/codegen/xai-grok-pager/docs/user-guide/17-sessions.md`) — diff that doc
  per release alongside the code.
- **Source pointers:** `crates/codegen/xai-chat-state/src/persistence.rs` (appends to
  `chat_history.jsonl`); session lifecycle under
  `crates/codegen/xai-grok-pager/src/app/dispatch/session/`. **Prior-art bonus:
  `crates/codegen/xai-grok-foreign-sessions/` is xAI's own Apache-2.0 Rust code that
  parses Claude Code and Codex session stores** (`src/claude/projects.rs`,
  `src/codex/db.rs`) — study it before writing rosetta's adapters.
- **Fixtures:** `grok -p "prompt" --output-format streaming-json` (NDJSON events
  `thought`/`tool_call`/`tool_call_update`/`text`; result JSON carries `sessionId`,
  `usage`, `total_cost_usd`); `-r`/`-c`/`--fork-session` for resume scenarios;
  `--always-approve`. Mock endpoint first-class: `[model.<name>]` blocks with
  `base_url`/`env_key`, or `GROK_MODELS_BASE_URL` (OpenAI-compatible gateway);
  `XAI_API_KEY` bypasses browser login. Fully hermetic.
- **Risks:** fast-moving beta; the plain-text version endpoint is the weakest watch
  format among Tier A agents (no feed metadata); source mirror may lag shipped
  binaries — treat fixtures, not the mirror, as decisive when they disagree.

### 5.10 Google Antigravity ("AGV") — **Tier C (CLI) / Tier D (IDE)**

Google's agentic platform, four surfaces: Antigravity 2.0 manager hub, the **IDE**
(VS Code fork, weekly releases), the **CLI** (`agy`, Go, near-daily patches), and an
SDK. **Closed source** (the GitHub repo carries no license and no source). The
roster's hardest target — plan for two lanes:

- **Track:** CLI — the install script's **Cloud Run JSON manifest**
  (`…/manifests/<platform>.json` returning `{version, url, sha512}` into a public GCS
  bucket) is a real programmatic channel; GitHub Releases mirror versions. IDE —
  scrape `antigravity.google/changelog` (the releases page is JS-rendered).
- **On-disk ground truth (macOS):** IDE agent data at `~/.gemini/antigravity/` →
  **moved to `~/.gemini/antigravity-ide/`** in newer builds: `conversations/*.pb`
  (**unpublished protobuf; newer versions use SQLite `.db` instead**),
  `brain/<uuid>/`, `knowledge/`; editor profile in
  `~/Library/Application Support/Antigravity[ IDE]/User/` with `state.vscdb` holding
  base64-protobuf trajectory summaries. CLI: config
  `~/.config/antigravity/config.toml`, settings `~/.gemini/antigravity-cli/settings.json`;
  CLI transcript location on disk UNVERIFIED (resume is by `--conversation <ID>` —
  possibly server-side). The user-observed `~/.antigravity` is likely the
  extensions/launcher dir (UNVERIFIED). Community export tooling reads conversations
  via **Antigravity's undocumented local HTTP API** rather than parsing `.pb` — that
  API is worth investigating as the capture path.
- **Fixtures:** CLI lane (Tier C): full headless surface exists — `agy -p`,
  `--output-format stream-json`, `--input-format stream-json` (stdin-driven
  multi-turn), `--json-schema`, `--dangerously-skip-permissions` — but auth is
  Google OAuth in the system keyring; no documented endpoint override
  (`ANTIGRAVITY_API_KEY` reported third-party, UNVERIFIED). Dedicated test account +
  keyring pre-seeding in a scheduled credentialed lane. IDE lane (Tier D): manual
  capture protocol — scripted checklist of in-IDE actions, then harvest
  `antigravity-ide/` + `state.vscdb`; investigate the local HTTP API for automation.
- **Risks:** highest in the roster — unpublished protobuf schemas, a data dir that
  already moved once, a `.pb`→SQLite storage migration mid-flight, OAuth-only auth,
  and weekly/near-daily churn on two surfaces. Scope Antigravity as
  **survey-and-monitor** initially: capture what the manual lane can, revisit
  automation when the local API is understood.

### 5.11 GitHub Copilot CLI — **Tier B**

GitHub's terminal agent (GA since Feb 2026). **Closed source** — the npm package is
an 8 KB loader shim over platform-native binaries in `optionalDependencies`; no JS to
diff. Multiple releases per month.

- **Track:** npm **`@github/copilot`** (authoritative; `@prerelease` variants exist);
  changelog.md in `github/copilot-cli` + github.blog posts.
- **On-disk ground truth (macOS):** everything under `~/.copilot` (**`COPILOT_HOME`
  override** — harvest isolation), and unusually, **officially documented**:
  `session-state/<session-id>/` with **`events.jsonl`** (prompts, responses, tool
  invocations, file modifications) plus plans/checkpoints; `session-store.db` is a
  **derived, rebuildable SQLite index** (`/chronicle reindex`) — exclude it from
  ground truth; `settings.json` (JSONC), `config.json`, `permissions-config.json`,
  `mcp-config.json`, `logs/`, `agents/`, `skills/`, `hooks/`. Field-level
  `events.jsonl` schema is NOT published — fixtures carry it.
- **Fixtures:** `copilot -p "prompt" -s` headless with `--allow-all`/`--allow-tool`
  granularity, `--share=PATH` (markdown transcript export — a free secondary
  surface). Hermetic thanks to **BYOK (since 2026-04)**:
  `COPILOT_PROVIDER_BASE_URL` + `COPILOT_PROVIDER_API_KEY` + `COPILOT_MODEL` against
  any OpenAI-compatible or Anthropic endpoint → mockmodel, no GitHub subscription in
  the loop. Keep one scheduled credentialed run against GitHub-hosted models to
  check BYOK/hosted parity of the event stream.
- **Risks:** `events.jsonl` schema undocumented and only inferable from fixtures;
  native binaries preclude static inspection; BYOK runs may not be byte-identical to
  hosted-model runs (sandbox/custom-agent features touch the event stream) — hence
  the parity lane.

### 5.12 Cross-agent patterns observed (first input to Phase 2)

Even before fixtures exist, the survey surfaces patterns the Layer-2 design must
absorb — and disproves some assumptions a Claude-only view would have baked in:

- **Storage tech is not converging on JSONL.** Append-only JSONL (Claude, Codex, Pi,
  Kimi, Grok, Copilot), SQLite as primary store (Hermes, OpenCode, Cursor), and
  rewrite-in-place JSON (Gemini legacy) all exist — and **three agents are
  mid-migration** (OpenCode JSON→SQLite, Codex JSONL→SQLite underway, Gemini
  JSON→JSONL). The capture layer needs three file disciplines: tail-append, whole-file
  reload, and SQLite WAL-aware polling. A JSONL-only tailer would already be wrong.
- **Schema versioning idioms differ**: per-record version stamps (Claude), an in-DB
  `schema_version` table (Hermes v23), a versioned session-header line (Pi v3),
  nothing explicit (most others). Rosetta's per-record `detected_version` must accept
  all three plus heuristics.
- **Universal shapes confirmed across all eleven**: session identity tied to a
  cwd/workspace key (five different encodings of the same idea); parent/child
  sessions and subagent trees; tool call/result pairing; token-usage blocks with
  cache splits; compaction/summarization events. These are strong candidates for the
  canonical model's spine.
- **Headless stdout streams are near-universal** (`stream-json`/NDJSON on 9 of 11) —
  a second, cheaper corroboration surface the fixture farm gets for free.
- **Prior art exists**: xAI's `xai-grok-foreign-sessions` crate (Apache-2.0) already
  parses Claude Code and Codex stores in Rust — read before writing those adapters.

## 6. Layer-1 exit criteria

Layer 1 is done — and Layer-2 design may begin — when, for every roster agent:

1. `watch.toml` exists with an authoritative channel and pinned version;
2. the layout manifest and versioned raw types are committed;
3. at least one fixture corpus (hermetic where Tier allows) is committed and replayed
   green in CI;
4. schema snapshots exist for the pinned version;
5. the drift pipeline has been exercised at least once (a release bump PR was produced).

Only then does Phase 2 (cross-agent pattern synthesis) freeze the canonical model.
