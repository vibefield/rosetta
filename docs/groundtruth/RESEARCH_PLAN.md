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

This is enforced, not merely recommended: fixture generation refuses to run on an
ordinary host. The container/VM runner image must be controlled by the project and
referenced by immutable SHA-256 digest in automation; conventional container markers
are detected automatically, and non-container isolation is explicitly attested with
`ROSETTA_FARM_ISOLATED=1`. `--allow-host-execution` exists only for disposable research
machines and emits a warning. An empty `$HOME` and per-agent data-root overrides isolate
the captured output, but are not a sandbox for freshly downloaded code.

**Mock model server.** One small standard-library Python server
(`tools/groundtruth/mock_model.py`) returning scripted responses per scenario
(including canned tool-call requests, streaming deltas, usage
blocks). The survey shows it needs **four wire dialects**: OpenAI Chat Completions
(OpenCode, Kimi, Pi, Hermes), **OpenAI Responses** (Codex accepts only
`wire_api = "responses"`), **Anthropic Messages** (Claude Code, Pi), and **Gemini
`generateContent`** (Antigravity CLI). Agents pointed at it via their base-URL override run
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
| Grok Build | **A** | `https://x.ai/cli/stable` (plain-text version) | `~/.grok/sessions/<cwd>/<uuid>/` (multi-file) |
| Antigravity | **B** (CLI) / **D** (IDE) | CLI: GitHub `google-antigravity/antigravity-cli` (+ Cloud Run manifest); IDE: changelog scrape | CLI: `~/.gemini/antigravity-cli/` (JSONL + SQLite) |
| Copilot CLI | **B** | npm `@github/copilot` | `~/.copilot/session-state/<id>/events.jsonl` |
| OpenCode | **A** | npm `opencode-ai` | `~/.local/share/opencode/opencode.db` (+legacy JSON) |
| Kimi Code | **A** | npm `@moonshot-ai/kimi-code` | `~/.kimi-code/sessions/…/agents/*/wire.jsonl` |
| Cursor CLI | **C** | changelog scrape (no artifact registry) | `~/.cursor/chats/…/store.db` + transcript JSONL |
| Hermes | **A** | GitHub `NousResearch/hermes-agent` releases | `~/.hermes/state.db` (SQLite, versioned schema) |
| Pi | **A** | npm `@earendil-works/pi-coding-agent` | `~/.pi/agent/sessions/…/*.jsonl` (versioned header) |

Nine of ten are hermetically fixture-able (Tier A/B). Only Cursor CLI (no endpoint
override) needs a credentialed lane; Antigravity IDE remains Tier D.

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
  identity `SOUL.md`, and **sessions in SQLite: `~/.hermes/state.db`**, schema tracked
  by an in-DB `schema_version` table (**v26** at 0.20.5): `sessions` (model,
  parent_session_id, token counts incl. cache_read/cache_write/reasoning, billing,
  cwd), `messages` (role, content, `tool_calls` JSON string, `api_content`
  byte-fidelity sidecar, compacted/active flags), `session_model_usage`,
  `system_prompts` (content-addressed), `state_meta`, three FTS5 tables,
  `gateway_routing`, `async_delegations`. Legacy JSONL in `~/.hermes/sessions/` is
  dead.
- **Source pointers:** `hermes_state.py` (`SessionDB`; WAL, `_reconcile_columns()`
  additive migrations, `export_session()`/`export_all()`); docs mirrored in-repo at
  `website/docs/developer-guide/session-storage.md`.
- **Fixtures:** headless is first-class — farm uses `hermes chat --query-file`
  `--yolo --quiet --ignore-rules --toolsets terminal --max-turns 8`, resume via
  `--resume <session_id>`. Custom endpoint CONFIRMED at 0.20.5:
  ```yaml
  model:
    default: mock-model
    provider: custom
    base_url: http://127.0.0.1:<port>/v1
    api_key: rosetta-mock-key
    context_length: 128000
  ```
  `--ignore-user-config` would skip this file — do not use it for mock routing.
  Because the store is binary SQLite, commit the `.db` harvest PLUS two diffable
  derivatives: `sources/<version>.sqlite-schema.sql` and per-table JSONL next to
  stdout (`sessions.jsonl`, `messages.jsonl`, …). Install: Docker image
  `nousresearch/hermes-agent:v2026.8.19`, falling back to `git clone --branch
  v2026.8.19` + `uv sync --frozen --no-dev` on CPython 3.13 (package
  `requires-python = ">=3.11,<3.14"`).
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
- **Captured (0.84.3):** writer-source intermediate at
  `groundtruth/pi/sources/0.84.3.writer.json` (session-manager.ts + messages.ts +
  pi-ai types). Fixtures: basic-chat (`session_info` via `--name`), tool-use
  (bash), resume, fork (`parentSession`), error (`stopReason=error`). Auto-compaction
  did not fire under the original 1024-token mock window; a subsequent pinned
  scenario captured compaction. Docs mention `retainedTail`; the 0.84.3 TypeScript
  `CompactionEntry` does not (channel-1 wins). Interactive-only types
  (`bashExecution`, `/tree` `branch_summary`, labels, abort) remain research.

### 5.3 OpenCode (Anomaly Co, ex-SST) — **Tier A**

Terminal agent (TUI + CLI + client/server), MIT, OSS. Originally SST, now developed
by **Anomaly Co** ([repo](https://github.com/anomalyco/opencode) — `sst/opencode`
redirects). Near-daily releases (surveyed `v1.18.23`; fixtures remain pinned at
`v1.18.22`).

- **Track:** npm registry `opencode-ai` `latest` dist-tag (GitHub `releases.atom`
  equivalent and carries changelog notes).
- **Install for farm:** `npm i -g opencode-ai@<pin>` (wrapper + platform binary deps).
  CONFIRMED 1.18.22: npm may skip `postinstall.mjs` even without `--ignore-scripts`;
  the farm re-runs it so `bin/opencode.exe` is the real Mach-O, not the stub script.
- **On-disk ground truth (macOS):** root `~/.local/share/opencode/` — `auth.json`,
  `log/`, and **SQLite `opencode.db`** (WAL). Docs still mention `project/<slug>/storage/`;
  1.18.22 hermetic runs write the db + logs, not that tree. **Storage migrated (~v1.2+)
  from per-record JSON (`storage/{session,message,part}/*.json`) to SQLite** —
  `OPENCODE_DB` overrides the path (harvest isolation), `opencode db path` prints it.
  Both formats exist in the wild; fixtures type the SQLite writer (session/project rows,
  `message.data` / `part.data`, `event.data` log). Legacy JSON streams stay research
  until a pre-1.2 corpus exists. Non-git cwds use `project.id=global`; git cwds hash the
  worktree and write `snapshot/<project>/<snap>/`. Config: `~/.config/opencode/opencode.jsonc`
  + per-project `opencode.json`. Farm isolation: empty HOME + `XDG_*` + `OPENCODE_DB`.
- **Source pointers:** `packages/opencode/src/storage/storage.ts` and
  `storage/schema.ts` (branch `dev`); session domain under
  `packages/opencode/src/session/`.
- **Writer closure (2026-08-28):** the 1.18.23 writer/schema export is committed as
  `groundtruth/opencode/sources/1.18.23.writer.json`. It covers the current SQLite
  tables and message/part unions, including reasoning, patch, retry, tool status/result
  and usage fields. This strengthens the 1.18.22 fixture corpus; it does not manufacture
  legacy pre-1.2 JSON evidence or external tool-output bodies.
- **Fixtures:** `opencode run "prompt" -m mock/mock-model --format json --auto`
  (json = raw event stream; `--auto` auto-approves; `-s`/`-c` resume; `--attach` to a
  running `opencode serve`). Endpoint override is first-class: declare a provider
  with `"npm": "@ai-sdk/openai-compatible"` and a local `baseURL` in `opencode.json`,
  no real credentials.
- **Risks:** near-daily releases → the release-watch will batch (e.g. weekly roll-ups)
  rather than PR-per-release; the JSON→SQLite migration is fresh with known bugs
  (issues #13654, #16885) and dual on-disk formats; the SQLite schema is internal,
  changeable without release-note callouts — schema-dump diffing is the only reliable
  drift signal; npm postinstall skip on prefix installs; possible further org/URL churn.

### 5.4 Kimi Code CLI (Moonshot AI) — **Tier A**

Moonshot's coding CLI — TypeScript successor to the Python `kimi-cli` (legacy being
wound down; `kimi migrate` imports old data). MIT, OSS
([repo](https://github.com/MoonshotAI/kimi-code)). Pre-1.0 (`0.38.0`), fast-moving.

- **Track:** npm registry `@moonshot-ai/kimi-code` (bin `kimi`). Changelog on
  kimi.com/code docs. Legacy PyPI `kimi-cli` only if historical coverage is wanted.
- **On-disk ground truth (macOS):** root **`~/.kimi-code/`** (`KIMI_CODE_HOME`
  override — harvest isolation). Documented (data-locations.md): `config.toml`,
  `tui.toml`, `mcp.json`, `AGENTS.md`, `skills/`, `plugins/`,
  `session_index.jsonl`, `credentials/` (0700/0600),
  **`sessions/<workDirKey>/<sessionId>/`** where `workDirKey =
  wd_<slug>_<sha256-12>`, per session `state.json`, **`agents/main/wire.jsonl`**
  (sub-agents `agents/agent-N/wire.jsonl`), `upcoming-goals.json`,
  `agents/*/plans/`, `tasks/`, `cron/`, per-session + global logs,
  `user-history/<md5(workdir)>.jsonl`, `bin/`, `updates/`. Writer + organic extras
  **not in the data-locations doc:** `workspaces.json`, `device_id`,
  `migrations-effort.json`, `agents/*/blobs/` (content-addressed `blobref:`
  offload), `agents/*/tool-results/`, `workspace-trust/<workDirKey>`,
  `region`, `oauth/` (distinct from `credentials/`), `cache/query-store/`
  (minidb, not SQLite), `cache/client-configs/`, `search-index/` (kap-server
  FTS), `telemetry/failed_*.jsonl`. `kimi web` additionally writes
  `server.token` (0600) and `server/` — not produced by headless `-p`. Legacy
  `~/.kimi/` layout differs (`context.jsonl` + `wire.jsonl`).
- **Source pointers:** `packages/transcript/` (wire.jsonl cold rebuild),
  `packages/agent-core-v2/src/wire/record.ts` (open `{type, time?}` envelope),
  `packages/agent-core-v2/src/app/event/event2.ts` (`durable` Event2 registry —
  58 durable types at 0.38.0; `agentId` is required in zod but organic
  protocol-1.5 sessions usually omit it; hermetic 0.38.0 fixtures include it),
  `packages/agent-core-v2/src/kosong/contract/message.ts` (`ContentPart` is
  `text | think | image_url | audio_url | video_url`) and
  `packages/agent-core-v2/src/tool/toolContract.ts` (`ExecutableToolOutput` is
  `string | ContentPart[]`),
  `packages/agent-core-v2/src/agent/tools/agent/` (Agent tool, profiles
  coder/explore/plan), official data-locations doc
  `docs/en/configuration/data-locations.md` (diff it per release — it lags the
  writer).
- **Fixtures:** `kimi -p "prompt"` (non-interactive; **implicitly auto-approves
  tools** — sandbox the runner), `--output-format stream-json`, `--session`/`-c`
  resume. `--plan` cannot combine with `-p`. Subagent farm uses the Agent tool
  (`[use-agent]` mock). Mock endpoint: **zero-config env synthesis — `KIMI_MODEL_NAME` +
  `KIMI_MODEL_API_KEY` + `KIMI_MODEL_BASE_URL` + `KIMI_MODEL_PROVIDER_TYPE=openai|anthropic|kimi`**
  creates a temporary provider, ideal for CI (note: regular provider keys are NOT
  read from env — they must be in `config.toml`; the `KIMI_MODEL_*` synth is the
  exception).
- **Risks:** pre-1.0 wire schema churn without semver signaling; mid-migration
  product with two data dirs (`~/.kimi` vs `~/.kimi-code`); three doc mirrors that
  disagree — treat kimi.com/code as canonical; headless auto-approval demands
  container isolation; OAuth vs API-key paths behave differently.
- **Adapter closure (2026-08-28):** `state.json` is now a first-class document snapshot
  that enumerates root and nested agents. The complete 0.38.0 fixture plus organic
  durable vocabulary is typed; bounded `state.json`/profile/config metadata supplies the one
  canonical session cwd while later `config.update` values remain transient tool-path context,
  `context.apply_compaction` supplies summary rows, and `tools.update_store` supplies
  tasks. Approval and lifecycle bookkeeping is intentionally a typed canonical no-op,
  so new fields still drift. Several organic-only variants still need writer-export or
  hermetic-fixture promotion.

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
- **Current gate:** the pack remains `blocked`, not “partially supported.” Until a funded,
  credentialed capture lane exists, Rosetta ships no Cursor adapter and does not substitute
  the unrelated IDE `state.vscdb` as evidence.

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
- **Organic census (2026-08-28):** 1,228,589 current transcript records were merged into
  the version-scoped canonical source. Custom-title records now participate in skim and
  summary projection. Metadata variants that intentionally emit no canonical row are
  still checked against their canonical top-level field sets, so declared no-ops no
  longer hide schema additions. Titles are not guaranteed to occur in the ordinary head
  or tail windows: the adapter may request a separately metered scan of complete records,
  capped at 128 KiB / 256 records, only while a main-session title is missing. Full
  streaming remains authoritative and reconciles generated and explicit title records;
  explicit titles win independently of source order or batch boundaries.

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
  `history.max_bytes`). Current installs also maintain **`state_*.sqlite`** under the
  data root. Its `threads` table is the native session-list metadata companion: `name`
  is the generated/user display name, while `title` is a prompt-like fallback; rollout
  headers themselves deliberately carry no display title. Rosetta therefore drives the
  rollout as JSONL and the state database as a metadata-only SQLite snapshot joined by
  native thread id. State schemas vary across releases, so the adapter interprets
  optional columns from `SELECT *` instead of hard-coding a query that mentions them.
- **Source pointers:** the current writer is split across
  `codex-rs/history/src/lib.rs`, `history/src/rollout_payload.rs`,
  `protocol/src/models.rs`, `protocol/src/protocol.rs`, and
  `rollout/src/policy.rs`; re-pin these paths when the crates move.
- **Writer closure (2026-08-28):** `openai/codex` tag `rust-v0.150.1` is committed as
  `groundtruth/codex/sources/0.150.1.writer.json`. The adapter covers the complete
  persisted top-level, response-item and event vocabulary, including compacted history,
  reasoning summaries/content, agent communication, realtime transcripts, tool search,
  web/image/local-shell records and current turn context. Fixtures remain pinned at
  0.149.1; an organic compaction replay is still desirable. The canonical layout admits
  `state_*.sqlite*` before the broader database inventory. Metadata projection prefers a
  normalized non-empty `name`, otherwise uses a whitespace-normalized `title` capped to
  36 Unicode scalar values, and leaves a genuinely blank title null.
- **Fixtures:** `codex exec "prompt" --json` (stdout = JSONL event stream). Mock via
  `[model_providers.mock]` in config.toml: `base_url` → mockmodel, `env_key` naming a
  dummy env var, **`wire_api = "responses"`** — the mock must speak the OpenAI
  Responses API, not chat-completions. No OAuth wall on this path.
- **Risks:** the JSONL/SQLite hybrid may move more authoritative state into the database
  mid-release; rollouts embed compaction history and can reach GBs — the scenario suite
  must include compaction; optional state columns and crate reorgs require the schema and
  `watch.toml` evidence to be refreshed together.

### 5.8 Grok Build (xAI) — **Tier A**

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
  (raw model messages: `system`/`user`/`assistant`/`tool_result` plus census
  `reasoning`/`backend_tool_call`), `plan.json`, `rewind_points.jsonl`,
  `signals.json`, `feedback.jsonl`, `compaction_checkpoints/`, `subagents/`,
  **`events.jsonl` (written by 1.0.5 even though user-guide 17-sessions.md omits
  it)**. Long cwds fall back to slug+hash with a `.cwd` file. Also `config.toml`,
  `auth.json`, an FTS5 SQLite search index, `worktrees.db`. The vendor documents
  the session layout in-repo
  (`crates/codegen/xai-grok-pager/docs/user-guide/17-sessions.md`) — diff that doc
  per release alongside the code. Confirmed 1.0.5 fixtures: shell tool is
  **`run_terminal_command`** (not `run_terminal_cmd`) and requires `command` +
  `description`. Organic census (2026-08-25, ~934k records) also saw
  `hook_execution`, `agent_thought_chunk`, subagent/compaction/plan updates,
  and MCP/goal event types.
- **Source pointers:** `crates/codegen/xai-chat-state/src/persistence.rs` (appends to
  `chat_history.jsonl`); session lifecycle under
  `crates/codegen/xai-grok-pager/src/app/dispatch/session/`. **Prior-art bonus:
  `crates/codegen/xai-grok-foreign-sessions/` is xAI's own Apache-2.0 Rust code that
  parses Claude Code and Codex session stores** (`src/claude/projects.rs`,
  `src/codex/db.rs`) — study it before writing rosetta's adapters.
- **Fixtures:** `grok -p "prompt" --output-format streaming-json` (NDJSON events
  `text`/`tool_call`/`tool_call_update`/`usage`/`end`/`available_commands`; the
  mock does not emit `thought` because it has no reasoning). Result JSON carries
  `sessionId`, `usage`. `-r`/`-c`/`--fork-session` for resume; `--yolo` auto-approves.
  Mock: `[model.mock-model]` `api_backend = "chat_completions"` +
  `GROK_MODELS_BASE_URL` + dummy `XAI_API_KEY`. Install via
  `https://x.ai/cli/grok-<ver>-<platform>` into an isolated dir — do not run
  `install.sh` (it mutates `~/.grok` and shell rc). Fully hermetic. Pin 1.0.5
  committed 2026-08-25.
- **Risks:** fast-moving beta; the plain-text version endpoint is the weakest watch
  format among Tier A agents (no feed metadata); source mirror may lag shipped
  binaries — treat fixtures, not the mirror, as decisive when they disagree.

### 5.9 Google Antigravity CLI (`agy`) — **Tier B (CLI) / Tier D (IDE)**

Google's agentic platform, four surfaces: Antigravity 2.0 manager hub, the **IDE**
(VS Code fork, weekly releases), the **CLI** (`agy`, Go, near-daily patches), and an
SDK. **Closed source** (the GitHub repo is docs/changelog/examples; the shipped
archive executable is named `antigravity`, installed as `agy`). This data pack is
the **CLI**. The IDE stays a separate survey-and-monitor lane.

- **Track:** CLI — GitHub Releases on `google-antigravity/antigravity-cli` (pinable
  `agy_cli_<os>_<arch>.tar.gz` assets with published SHA-256). The install script's
  **Cloud Run JSON manifest** (`…/manifests/<uname>_<arch>.json`, e.g. `darwin_arm64`,
  returning `{version, url, sha512}` into a public GCS bucket) is the live
  auto-updater channel; poll it for "a new version exists", farm from the GitHub tag.
  IDE — scrape `antigravity.google/changelog` (the releases page is JS-rendered).
- **On-disk ground truth (macOS, CLI):** `~/.gemini/antigravity-cli` (no documented
  home-env override — isolate via empty `HOME`). Primary rails: append-only
  `brain/<conversation-id>/.system_generated/logs/transcript.jsonl` and sibling
  `transcript_full.jsonl`; WAL-mode SQLite `conversations/<id>.db` (+ `-wal`/`-shm`);
  `conversation_summaries.db`; `cache/last_conversations.json` (cwd → UUID for `-c`);
  `settings.json`. Resume is `--conversation <uuid>` or `--continue`/`-c`. Hook docs
  name `transcriptPath` at the full JSONL. IDE agent data (`~/.gemini/antigravity/` →
  `~/.gemini/antigravity-ide/`, unpublished `.pb` then SQLite) is **not** this pack.
- **Source pointers:** none (Go binary; GitHub tag is release provenance, not
  source-to-binary correspondence). Ground truth = fixtures + docs + binary strings.
- **Fixtures:** `agy -p "prompt" --output-format stream-json --print-timeout 2m
  --dangerously-skip-permissions`. Auth for the farm is the documented **Gemini
  Developer API** path: `settings.json` `modelProvider = "gemini"` + dummy
  `GEMINI_API_KEY` + `GOOGLE_GEMINI_BASE_URL` → mockmodel speaking Gemini
  `generateContent` / `streamGenerateContent`. Default Google OAuth/keyring is the
  interactive path and is not used by the farm. Harvest immediately; strip
  `bin/`, `builtin/`, `log/`, `updater/`, `crashes/`, `presence/`,
  `cache/CHANGELOG.md`.
- **Risks:** near-daily CLI patches; SQLite blob payloads inside `steps` are not
  JSON; transcript vs SQLite dual rails; `modelProvider=gemini` is required or
  the CLI ignores `GEMINI_API_KEY` and falls back to OAuth; IDE remains Tier D.
  Print-mode farm does not write `history.jsonl` or `implicit/*.pb`; those are
  observed/research. Organic census (2026-08-25) is the evidence for
  CONVERSATION_HISTORY / LIST_DIRECTORY / VIEW_FILE / ERROR_MESSAGE.
- **Metadata/title closure (2026-08-28):** `conversation_summaries.title` is an
  explicit display label and `preview` remains semantically distinct summary data.
  When that database is sparse, a complete checkpoint may supply only the exact
  `# USER Objective:` section as a lowest-confidence fallback: stop at the next
  Markdown heading, normalize whitespace, reject blank/malformed sections, and cap
  the display value to 80 Unicode scalar values including an ellipsis. Meta skim
  chooses the greatest visible `step_index` from the head/tail records and may use
  the shared 128 KiB / 256-record bounded fallback only while the title is absent;
  full streaming emits the same source-owned fallback and therefore repairs deeper
  objectives. Explicit database titles always outrank checkpoint fallbacks, while
  preview-only sessions stay untitled. Normalizer 4, canonical-op goldens, parser
  unit tests, and pipeline replay/incremental tests freeze those rules.

### 5.10 GitHub Copilot CLI — **Tier B**

GitHub's terminal agent (GA since Feb 2026). **Closed source** — the npm package is
an 8 KB loader shim over platform-native binaries in `optionalDependencies`; no JS to
diff. Multiple releases per month.

- **Track:** npm **`@github/copilot`** (authoritative; `@prerelease` variants exist);
  changelog.md in `github/copilot-cli` + github.blog posts.
- **On-disk ground truth (macOS):** everything under `~/.copilot` (**`COPILOT_HOME`
  override** — harvest isolation), and unusually, **officially documented**:
  `session-state/<session-id>/` with **`events.jsonl`** (prompts, responses, tool
  invocations, file modifications), a small flat **`workspace.yaml`** carrying the native
  session id, cwd, generated/user name and timestamps, plus plans/checkpoints;
  `session-store.db` is a
  **derived, rebuildable SQLite index** (`/chronicle reindex`) — exclude it from
  ground truth; `settings.json` (JSONC), `config.json`, `permissions-config.json`,
  `mcp-config.json`, `logs/`, `agents/`, `skills/`, `hooks/`. Field-level
  `events.jsonl` schema is NOT published — fixtures carry it.
- **Fixtures:** committed at **1.0.80** (`basic-chat`, `tool-use`, `resume`,
  `tool-denied`) via `copilot -p "prompt" --output-format json --allow-all
  --no-ask-user --share=share.md` against the Chat Completions mock.
  `--continue` appends `session.resume` to the same `events.jsonl`.
  `--deny-tool=shell` writes `tool.execution_complete` with `success: false` and
  `error.code=denied` (no `permission.*` events — deny-by-rules skips the prompt).
  `--share` markdown is a farm artifact next to stdout, not part of `~/.copilot`.
  Writer JSON Schema is copied from the platform npm package
  (`sources/vendor/<ver>.session-events.schema.json`) and folded in as
  `writer` evidence (~53 persisted types; ~63 ephemeral types omitted).
  Hermetic thanks to **BYOK (since 2026-04)**:
  `COPILOT_PROVIDER_BASE_URL` + `COPILOT_PROVIDER_API_KEY` + `COPILOT_MODEL` against
  any OpenAI-compatible or Anthropic endpoint → mockmodel, no GitHub subscription in
  the loop. Keep one scheduled credentialed run against GitHub-hosted models to
  check BYOK/hosted parity of the event stream. Field-level on-disk schema is the
  merge of that writer schema with fixtures — not either source alone.
- **Metadata closure (2026-08-28):** `events.jsonl` remains the only timeline rail;
  `workspace.yaml` is a metadata-only `DocumentSnapshot` capped at 64 KiB. The parser
  accepts only the writer's flat scalar form, rejects a body/path session-id mismatch,
  and preserves the previous projection on malformed partial rewrites. Rename updates
  replace the native title without consulting the derived `session-store.db`.
- **Risks:** `events.jsonl` schema undocumented and only inferable from fixtures;
  native binaries preclude static inspection; BYOK runs may not be byte-identical to
  hosted-model runs (sandbox/custom-agent features touch the event stream) — hence
  the parity lane.

### 5.11 Cross-agent patterns observed (first input to Phase 2)

Even before fixtures exist, the survey surfaces patterns the Layer-2 design must
absorb — and disproves some assumptions a Claude-only view would have baked in:

- **Storage tech is not converging on JSONL.** Append-only JSONL (Claude, Codex, Pi,
  Kimi, Grok, Copilot), SQLite as primary store (Hermes, OpenCode, Cursor), and
  rewrite-in-place JSON sidecars (plans, summaries, and indexes) all exist — and
  **two agents are mid-migration** (OpenCode JSON→SQLite and Codex JSONL→SQLite).
  The capture layer needs three file disciplines: tail-append, whole-file
  reload, and SQLite WAL-aware polling. A JSONL-only tailer would already be wrong.
- **Schema versioning idioms differ**: per-record version stamps (Claude), an in-DB
  `schema_version` table (Hermes v23), a versioned session-header line (Pi v3),
  nothing explicit (most others). Rosetta's per-record `detected_version` must accept
  all three plus heuristics.
- **Universal shapes confirmed across all ten active agents**: session identity tied to one
  primary cwd/workspace metadata fact (five different encodings of the same idea); parent/child
  sessions and subagent trees; tool call/result pairing; token-usage blocks with
  cache splits; compaction/summarization events. These are strong candidates for the
  canonical model's spine.
- **Headless stdout streams are near-universal** (`stream-json`/NDJSON on 9 of 10) —
  a second, cheaper corroboration surface the fixture farm gets for free.
- **Prior art exists**: xAI's `xai-grok-foreign-sessions` crate (Apache-2.0) already
  parses Claude Code and Codex stores in Rust — read before writing those adapters.

## 6. Layer-1 exit criteria

Each `watch.toml` explicitly marks its pack `captured` or `blocked`; absence of a
canonical document is never treated as success. Layer 1 is done — and Layer-2
design may begin — when no roster agent remains blocked and, for every agent:

1. `watch.toml` exists with an authoritative channel and pinned version;
2. the layout manifest and versioned raw types are committed;
3. at least one fixture corpus (hermetic where Tier allows) is committed and replayed
   green in CI;
4. schema snapshots exist for the pinned version;
5. the drift pipeline has been exercised at least once (a release bump PR was produced).

Only then does Phase 2 (cross-agent pattern synthesis) freeze the canonical model.
