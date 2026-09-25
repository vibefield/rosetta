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
fixture generation CI-able. A prompt carrying `[usage]` makes every dialect report
distinct non-zero cache reads, 5-minute and 1-hour cache writes, reasoning, web searches
and xAI billed ticks; each agent's `usage` scenario runs its tool loop with that marker,
so the capture proves how the agent folds each field (§5.13).

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
- **Organic census (2026-08-28):** 1,228,589 retained transcript records were merged into
  an all-writer-version corroboration source. Custom-title records now participate in skim and
  summary projection. Metadata variants that intentionally emit no canonical row are
  still checked against their canonical top-level field sets, so declared no-ops no
  longer hide schema additions. Titles are not guaranteed to occur in the ordinary head
  or tail windows: the adapter may request a separately metered scan of complete records,
  capped at 128 KiB / 256 records, only while a main-session title is missing. Full
  streaming remains authoritative and reconciles generated and explicit title records;
  explicit titles win independently of source order or batch boundaries.
- **Delegated-work census (2026-09-03):** the privacy-bounded, all-retained-version
  refresh at `groundtruth/claude-code/sources/observed-2026-09-03.json` scanned
  1,300,340 JSON/JSONL records across eleven admitted streams. It adds the two omitted
  workflow rails (`workflows/wf_*.json` and `subagents/workflows/*/journal.jsonl`) and
  content-free facets for Agent launch/resume, SendMessage routing, response model/effort,
  structured origins, top-level agent/team/task envelopes, team members, and inbox items.
  This is Channel-4 corroboration, not fixture authority; its job is to prevent the
  presentation and adapter work from collapsing three distinct persisted relationships:
  invocation-owned workers, persistent team members, and workflow-managed worker sets.
  The same audit found 258 `origin.kind=peer` user records (251 wrapped
  `agent-message` deliveries and seven wrapperless deliveries) plus 25 coordinator
  follow-ups, so ordinary subagents are not a one-request/one-result-only channel.
  Four `Agent(resume=...)` requests returned `teammate_spawned`, and 88 SendMessage
  results carried `resumedAgentId`; result status alone therefore cannot distinguish a
  new member from a resumed one. Of 3,220 literal `teammate-message` tags, only 1,904
  are top-level envelopes; 1,316 are nested copies inside another notification/result
  body and cannot be projected as separate chronology. The refresh now facets the Workflow
  rails themselves: 119 phases and 761 current workers across 48 run snapshots, all four
  observed worker states, 14 cached/resume-reused workers, configured models, arbitrary run
  result kinds, exact journal result kinds, 35,545 Workflow-child transcript records, and
  Workflow-only launch receipts and callbacks. The correlated audit additionally proves that
  current progress is a replaceable roster, journals accumulate untimestamped attempt
  evidence, callbacks are attempt-scoped and optional, and a resumed run can finish after its
  first callback failed. Full relationship, lifecycle, and de-duplication constraints are
  catalogued in `docs/groundtruth/claude-code.md` §4.

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

**Product admission policy (2026-09-09).** Internal approval reviewers are excluded
from the canonical session/timeline/search projection during fresh ingest. Match
native `source.subagent.other == "guardian"` or `thread_source == "guardian_review"`
in both rollout headers and state metadata; the generic `subagent` label, a review
title, or prose mentioning guardian is insufficient. A local structure/count audit
found 193 such threads (143 with the older generic thread-source label), but does not
serve as CI input or expand the native schema. Synthetic policy tests exercise the
existing permissive source field, checkpoint restoration, and negative cases for
ordinary review/delegated work. Hermetic pipeline tests prove source bytes remain in
the ledger while metadata, timeline rows, text assets, usage and search stay absent,
including fresh parallel preparation and replay of the newly captured ledger.

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
  **derived, rebuildable SQLite index** (`/chronicle reindex`) for titles and search —
  excluded from ground truth — except its **`assistant_usage_events`** table (CLI ≥ 1.0.69),
  the only per-request record of metered usage, which `/chronicle reindex` does not rebuild:
  the farm keeps that table and the session keys, and the adapter reads it as a
  `SqliteSnapshot` (§5.13); `settings.json` (JSONC), `config.json`, `permissions-config.json`,
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

### 5.12 Authored image audit (2026-09-09)

**Implemented against known writer fields; native capture coverage remains incomplete.** The current registered roster has
nine adapters. Seven have a known authored-image representation in their canonical
pack or pinned writer; that does not establish complete fixture or normalization
coverage. The other two have an evidence gap, not evidence that the product cannot
accept images. Cursor remains uncaptured. The common contract is in
[ARCHITECTURE.md §4.1.2](../ARCHITECTURE.md#412-authored-images-2026-09-09);
the owning UI record is
[§06A](../../clients/app/design/transcript-record.html#prompt-images).

| Adapter / evidence reviewed | Authored-image evidence | Implemented projection | Remaining native evidence |
|---|---|---|---|
| [Claude Code 2.1.241](../../groundtruth/claude-code/groundtruth.json) | User image blocks with inline Base64 or URL sources | Shared validated assets or references; invalid slots survive. Team/injected/tool media is excluded from user uploads. The 2.1.267 writer’s `imagePasteIds` binds numbered markers to image blocks. | Capture an isolated prompt with inline, URL and omitted media; native paste binding is writer-proven and covered by synthetic conformance tests. |
| [Codex 0.149.1 fixtures / 0.150.1 writer](../../groundtruth/codex/groundtruth.json) | Pinned writer's `ContentItem::InputImage` with `image_url`; local-image text delimiters surround the actual image block | Refreshed content-union facet; data URLs become assets. Only exact opener/image/closer sequences lose transport text. Source labels emit unambiguous Unicode-scalar spans. | Commit an isolated native image scenario, including local and remote inputs; conformance tests and the private local corroboration are not a harvested fixture. |
| [Pi 0.84.3](../../groundtruth/pi/groundtruth.json) | Typed user `image` blocks with `data` and `mimeType` | Correct native MIME field binding; text and ordered image slots share one prompt. Invalid first slots and repeated bytes survive. | Capture text+image and image-only native sessions. |
| [Grok 1.0.5](../../groundtruth/grok/groundtruth.json) | Observed `user_message_chunk` image content includes `data`, `mimeType`, and `uri`; prompt identity is in update metadata | Existing prompt accumulator now retains ordered attachments, inline assets and references across checkpoints. | Capture image/text chunk ownership, ordering and reference-only content. |
| [OpenCode 1.18.22 / writer 1.18.23](../../groundtruth/opencode/groundtruth.json) | Message file parts with `mime`, `filename`, `url`, and optional source metadata | Exact message-id assembly produces one user prompt; native part ids remain stable attachment ids. Data URLs become assets, other URLs remain references. Fresh snapshot replay removes deleted parts. | Capture mixed file/text parts and native snapshot replacement/deletion. |
| [Copilot 1.0.80](../../groundtruth/copilot/groundtruth.json) | `user.message.attachments` variants include inline data, asset ids, and omission metadata; `session.binary_asset` stores separately identified bytes | Inline/reference/omitted slots; exact session-scoped asset joins before or after the owner; checkpoints keep refs, not encoded bytes. Conflicting ids or byte metadata fail closed. | Capture the image variants and binary assets before/after the owner in native sessions. |
| [Kimi Code 0.38.0](../../groundtruth/kimi-code/groundtruth.json) | `image_url` content parts with an `imageUrl` reference | Recorded data URLs can materialize; blob/remote locators remain references. Authored media retains ownership; tool/injected media does not become a user image. | Capture the URL encodings actually persisted by an image-input session. A tested data URL parser is not proof the writer emits one. |
| [Hermes 0.20.5](../../groundtruth/hermes/groundtruth.json) | No authored-image field established by the captured pack reviewed here | No image attachments projected | Capture a pinned image-input scenario and inspect the writer's persistence path. Do not infer unsupported capability from a text-only corpus. |
| [Antigravity 1.1.20](../../groundtruth/antigravity/groundtruth.json) | No authored-image field established by the captured pack reviewed here | No image attachments projected | Capture the input-media scenario for the enrolled CLI; keep IDE evidence separate. An image-generation tool name does not prove prompt-image persistence. |

**Claude numbered-image links.** The 2.1.267 native writer carries `imagePasteIds`
through user-message construction and pairs its entries with top-level image blocks
using an image-only counter. The binding is recorded in the `image-paste-bindings`
facet, sourced from `sources/2.1.267.image-paste-writer.json` with the inspected
binary’s checksum. IDs are session-managed numbers, so the first image may be
`[Image #7]`; local attachment order alone does not establish its label. The adapter
now projects labels and exact final-text Unicode-scalar ranges only for complete,
unique integer ID lists. Missing image bytes keep their slot; documents do not consume
an image ID. A local failing prompt corroborated `[1]` alongside one stored image,
with no canonical label/ranges before this fix. No private prompt or image is committed.
Normalizer version 27 causes installed projections to replay quiet Claude sources on
the next daemon catch-up; the wire shape and projection schema are unchanged.

**Confirmed Codex failure.** A local 0.153.4 rollout corroborates the writer: a user
message contains an opening image-label/path text block, an `input_image` data URL,
a closing text block, and the user's question. The PNG is actually embedded (235,791
decoded bytes, 1090×704); this is not a lost temporary-file problem. The previous
adapter emitted only the text, explaining the visible wrapper and missing preview.
No local transcript, prompt text, image, or private path is committed as fixture
evidence. The new pipeline test proves ledger-only recovery into fresh asset members
after deleting its synthetic native files and original assets.
The opt-in check of the selected local record also passed: it rebuilt the same
235,791 bytes and 1090×704 dimensions from the ledger alone. The test prints only
those facts; private source paths and content are supplied at invocation time.

Pinned primary-source checks:

- [Codex `models.rs`, rust-v0.150.1](https://github.com/openai/codex/blob/rust-v0.150.1/codex-rs/protocol/src/models.rs)
  declares `InputImage`, local-image label/open/close helpers, and tests for mixed
  local/remote images. The path in the opener is not XML-escaped, so this is a native
  block-sequence format, not generic XML to parse in the frontend. A quoted opener
  with no actual image block must remain ordinary prose.
- [Pi `types.ts`, v0.84.3](https://github.com/earendil-works/pi/blob/v0.84.3/packages/ai/src/types.ts)
  declares user image content as Base64 data plus MIME type.
- [OpenCode session schema, v1.18.23](https://github.com/anomalyco/opencode/blob/v1.18.23/packages/schema/src/v1/session.ts)
  declares file parts independently of text parts, making exact message assembly
  part of image support rather than a rendering detail.

Add `prompt-images` to the farm's capability matrix. Use small generated test images
with known byte digests, not organic screenshots. Each applicable pack needs text+
image, image-only, several images, repeated-image occurrences, missing/invalid first
image, native labels, Unicode/path punctuation, and literal-wrapper negative cases;
add tool-result exclusion and reference-only cases where native writers support them.
Fragmented stores additionally need exact owner joins, replacement/deletion, late
bytes, replay, and cross-session-id collision cases. Synthetic malformed-input Rust
tests supplement genuine captures; they do not masquerade as writer evidence.
The seven applicable `watch.toml` files explicitly retain the native-capture gap.
The Codex facet was regenerated with `build_canonical` from its source intermediate
and rendered; no canonical JSON was hand-edited. Other adapters use their existing
canonical fields. Run boundary/fixture checks whenever these packs change.

### 5.13 Usage accounting audit (2026-09-22, fixes 2026-09-24)

**Implemented against pinned writer source and aggregate census counts, then audited
agent by agent on 2026-09-23 and fixed on 2026-09-24.** The native fixture capture is still
pending. This section owns the per-agent rules behind
[ARCHITECTURE.md §4.1.3](../ARCHITECTURE.md#413-usage-accounting-and-list-price-usd-2026-09-22).
Each adapter maps its native usage into the six mutually exclusive token buckets and
the usage facts, and a subset larger than its total is clamped and reported as drift. The
2026-09-23 audit (nine parallel reviewers, one per agent plus the shared catalog, calculator,
rollup and app) found the arithmetic exact everywhere and the errors in coverage and
counting; each row below records what was fixed and what a native capture must still cover.
Several writers moved during the audit (Kimi Code 2.0.2, Hermes 0.21.4, Pi 0.87.1, Copilot
1.0.88, OpenCode 2.0.15, Codex 0.155.1): the rules were checked against those sources and the
pins stay where the fixtures are.

The evidence comes from three places:

- pinned writer source for open agents;
- installed binaries and bundles for closed agents;
- read-only census counts of James's local data (numbers only, never content).

The committed fixtures were farmed against a mock that reported zero cache and
reasoning, so they prove field shapes, not these subset rules. The mock's `[usage]`
marker and each agent's `usage` scenario (§2) close that gap once an isolated runner
captures them. Until then, the synthetic tests in each adapter pin the pinned grammar.
They do not replace native captures.

| Agent (pinned) | Usage record | Native inclusion | Exclusive mapping and facts | Now handled | Still open |
|---|---|---|---|---|---|
| Claude Code 2.1.241 (rules re-checked on 2.1.280) | `message.usage` on every streamed block record of one response. Only the final record (non-null `stop_reason`, `speed` key) carries the response's real usage; it is the last line in 147,886 of 147,888 multi-line responses. Writers through 2.1.7 wrote one progressive count per record whose last record is the real output; from 2.1.9 every non-final record repeats the `message_start` placeholder (1–92 output tokens), and until 2.1.50 no final record was written at all. | `input_tokens` excludes cache. `cache_creation_input_tokens` = 5-minute + 1-hour writes (212,252 of 212,257 responses). `output_tokens` includes `output_tokens_details.thinking_tokens` (0 violations). | `cache_write_1h` = min(1h, total), taken from the serving `fallback_message` entry on a fallback turn. `thinking` = min(thinking, output). `service_tier`: `speed: "fast"` gives fast, otherwise `service_tier`. `route` = `anthropic/<inference_geo>` unless the geo is `not_available`, `global` or empty. `web_searches` from `server_tool_use`. Never billed; cost-state figures are estimates. | The most final snapshot wins and a later placeholder never regresses it. Every usage fact of a response is a `Usage` op behind its owning row (never inside it) or a `response:<id>` sample, and carries the transient identity `<message.id>:<requestId>` (the message id alone on 16 pre-2.1.9 records); iteration samples and retractions carry it too, and the store keeps the earliest-started session's copy (3,180 responses copied into up to 21 files, 10,761 extra copies, $1,609). A non-final snapshot on a writer from 2.1.9 reports `output` and `thinking` as unrecorded and the final record replaces it with the real count: 32,460 responses, 251,528 placeholder tokens ($6.24) leave the totals, while the ≈31.7M never-recorded output tokens stay out of coverage and are counted by the `unrecorded_output` rollup. A fact held in a `response:` sample moves to the response's first visible row as soon as one exists and the sample is retracted with an all-None snapshot: 98,397 of 98,481 samples ($37.5K, 64% of Claude's cost) now sit on rows and reach reply heads; 84 responses never project a row. A replayed record (every block already projected) carrying the known snapshot re-states the fact, so an in-file replay cannot wipe it (7 responses, $0.56), and the store's row upsert leaves usage columns alone when a re-emitted row carries none. `<synthetic>` error records carry no usage. Advisor and compaction `iterations` entries become samples of their own; several `message` entries are an aggregate with `requests` = their count. | WebSearch runs in a separate request that is never recorded (every `web_search_requests` is 0); title, summary, away-recap and compaction calls are unrecorded: Claude Code's own counters run 2–6.5% above the transcripts. Locally `speed` is only ever `standard`, `inference_geo` only `not_available`, and the only non-`message` iteration is `fallback_message` (7 responses): fast mode, residency, advisor and compaction iterations await a native capture. |
| Codex 0.149.1 (records checked on 0.155.1) | `event_msg` `token_count` after each request: `total_token_usage` is the thread's running sum and `last_token_usage` the request (`protocol/src/protocol.rs:2092-2131`). From 0.153 a `token_usage_record` per completed response is persisted just before that response's `token_count` (`core/src/session/mod.rs:4645-4675`, `turn.rs:2850-2863`); a remote compaction writes only the record (`compact_remote_v2.rs:462-477`). | `input_tokens` includes cached reads and cache writes (`codex-api/src/sse/responses.rs:122-161`; Codex's own test 100 = 40 + 60 at `:864-891`). `output_tokens` includes `reasoning_output_tokens`. | Per request: `cache_read` = min(cached, input), `cache_write` = min(writes, rest), `input` = the rest, `thinking` = min(reasoning, output). Tier from `thread_settings_applied` (`fast`/`priority` → priority, `flex`, `default` → standard): a missing `service_tier` key is unset because Codex omits it (`protocol.rs:2200-2201`), an unknown value is drift and stays absent. Codex records no tier for a session's first turn, so the first snapshot's tier is back-applied to the file's earlier requests as an estimate of the configured tier, never the served one (7,499 requests, +$277 locally). `route` = `session_meta.model_provider` unless `openai`. `web_searches` = `web_search_call` items with action `search`. | Records are primary where present, keyed by `response_id`; the `token_count` twin is skipped by usage equality (9,015 of 9,107 locally) and 92 remote compactions are counted only from records (+$129). Counter rule otherwise unchanged, except that a total advancing by more than `last` counts `last` as the request and the remainder as an uncounted aggregate. Replay gates skip only the ≤ 1 s head burst below `subagent_history_start_ordinal` or after a fork, spawn or parent header: guardian rollouts and `legacy_to_paginated_v1` migrations place the boundary past their last record (208 files locally), and a genuine prefix is a burst with no readings. Only a file's first header configures it. Guardian (`codex-auto-review`) requests attribute to the parent named by `parent_thread_id` as `session_usage` samples: 3,592 requests, 325 M tokens locally, unpriced but visible; a review whose parent was never captured stays out of coverage. | A subagent file with no settings snapshot inherits no tier (35 files locally; HEAD strips the parent's snapshots from the prefix): cross-file join. `codex-auto-review` and `gpt-5.3-codex-spark` have no list price. A child's own request completing within 1 s of its header is still skipped. gpt-5.6-sol's $4/$20 is OpenAI's promotional list price "at least through November 21, 2026": recheck then. Native captures pending: a record and its twin, a remote compaction, a guardian review with its parent, a paginated spawn, a configured tier. |
| Grok Build 1.0.5 (ids checked on 1.0.41) | `updates.jsonl` `turn_completed.usage`: per prompt, all main-loop calls, with one `modelUsage` entry per served model (`xai-grok-shell/src/extensions/notification.rs:106-126`; the per-prompt ledger is cleared at `xai-chat-state/src/actor/mutations.rs:507-509`). | `inputTokens` is the full prompt: uncached + cache reads + cache writes (`notification.rs:208,218-219`). `outputTokens` includes `reasoningTokens` (`:189`, wire test `xai-grok-sampler/src/client.rs:3316-3321`; 0 of 634 local violations). | One record per `modelUsage` entry; aggregate unless `modelCalls` = 1, with `requests` = `modelCalls`. `billed_pico_usd` = `costUsdTicks` × 100 (10¹⁰ ticks per USD, the server's own charge, removed when partial). | Each served model prices separately. A prompt without a response row still counts, as a sample. `grok-4.6-build`, `grok-4.5-build` and (from 2026-09-23) `grok-4.7-build` price as the listed grok-4.6, grok-4.5 and grok-4.7: on every single-request record, `costUsdTicks` equals those list prices to the tick, including the 200K tier, until a Grok Build discount began in mid-August 2026 (0.17× on 4.6, 0.34× on 4.5, ≈0.5× on 4.7). `grok-4.7-build-fast` is xAI's Grok 4.7 Fast, its own price table at twice the standard rates. xAI's $5 per 1,000 web searches is priced on the grok-4 line. | A parent turn folds finished children's spend in while the child sessions also record it: 6 of 17 local parents fold exactly, 9 fold nothing. A fork copies `updates.jsonl`. Both need the cross-file join; the usage identity mechanism now exists but Grok's fold is a sum, not a copied record. `usage.json` duplicates the ledger and is never read. |
| Copilot CLI 1.0.80 (store schema 6; 1.0.88 DDL reviewed) | Two records. `~/.copilot/session-store.db` `assistant_usage_events`: one row per model request, written live by the native runtime's usage processor from the ephemeral `assistant.usage` event since CLI 1.0.69 ("Show exact local assistant usage in Chronicle and session SQL"), never updated or deleted, and not rebuildable by `/chronicle reindex`. `session.shutdown` `modelMetrics`: cumulative per session × model, re-reported on resume (the resume fixture goes 128 → 256). | Row and shutdown alike: `input_tokens` includes cache reads and writes (locally 19,918 = 3 + 0 + 19,915 per call, 136,067 = 15 + 76,154 + 59,898 at shutdown), `output_tokens` includes `reasoning_tokens`. 1.0.88 folds 1-hour writes into `cache_write_tokens` and lists them as a `cache_write_1h` line item in `token_details_json`. `total_nano_aiu` is CAPI's charge, 10⁹ nano-AIU per AI credit ($0.01); 0 for BYOK. | Per metered row (`total_nano_aiu` > 0): one request sample at `created_at`; `input` = total − reads − writes, `output` = total − reasoning, `cache_write_1h` = the line item and `cache_write` = the column minus it, `billed_pico_usd` = nano-AIU × 10, model = `copilot_usage_model` else `model`, with `-1m`/`-1m-internal` → `[1m]` and `-fast` → `service_tier` fast. Per unmetered shutdown interval, or any interval of a writer below 1.0.69: one aggregate sample per model with `requests` = Δ`requests.count`; a reset is decided on the four required counters only, and absent optional counters stay unknown. | One call is never counted twice: metered calls come only from store rows, unmetered usage only from shutdown intervals, each side deciding from its own record. Float `totalNanoAiu` and `totalPremiumRequests` parse. Locally the five rows sum to the shutdown reading and price to its $0.1865338 exactly, row by row. Mixed-model intervals are one sample per model; a call whose line items name several models keeps its billing model and is reported as drift. | A native capture of the store (the farm now keeps `assistant_usage_events` and the session keys), BYOK and hosted. Until core prefers rows per session: a metered session whose rows were lost, the unmetered calls of an interval that also metered some, and a pre-1.0.69 session resumed later have no record. 1.0.88 `agentMetrics` and fused multi-model rows are unobserved. |
| Kimi Code 0.38.0 (rules checked on the 2.0.2 bundle) | `wire.jsonl` `usage.record`, one per finished request, written by `LlmRequesterService` after the stream's finish event (kimi@103495669) with `usageScope: "turn"` for loop steps and `"session"` for every other request (`UsageAgentModel.record` kimi@101743598). | Exclusive by design: `inputOther = prompt_tokens − cached_tokens` (`parseOpenAIUsage` kimi@98617262). The OpenAI-compatible providers fold cache writes into `inputOther` and never split reasoning out. | 1:1; `thinking` unreported. A negative `inputOther` is clamped at zero with drift, never a parse failure. The four optional 2.x `llm.request` fields no longer hard-fail the request, and the sample takes the request's API model id (`k3`), not the config alias (`kimi-code/k3`). | The first VISIBLE row of the same step owns the request (a text part with content, a media part or a tool call, never a think part), and whatever is still pending when the step closes becomes a sample keyed by the step uuid, so no request lands on an invisible row (33.6% of Kimi's cost was off the transcript) or strands at EOF. A 2.x fork byte-copies every wire: a record older than the fork instant (`state.json` `forkedFrom` + `createdAt`, read by the meta skim) keeps its rows and drops its tokens. `usageScope: "session"` records are billable non-loop requests (compaction), counted as samples. Two turn records before one response accumulate. The `forked` record type is declared. | `k3-256k`, `kimi-for-coding` and `kimi-for-coding-highspeed` price through the catalog aliases; `kimi-for-coding` is unpriceable at list from 2026-09-11 (K2.8 Preview has no per-token price). K3's 1-hour writes are priced through the overlay patch ($6.00). Native 2.x captures (fork, optional request fields, compaction) are pending. |
| Hermes 0.20.5 (rules checked on 0.21.4) | `state.db` `session_model_usage`, a running total per session × model × provider × base URL × mode × task (`hermes_state_usage.py:52-71`; 0.20.5 `hermes_state.py:8758-8777`), upserted per API call; named profiles keep their own `profiles/<name>/state.db`, now a stream of its own. | `input_tokens` is uncached (`agent/usage_pricing.py:535-539`). `reasoning_tokens` is inside `output_tokens` (`:541-546`); the Anthropic and Bedrock wires never split it out and the Gemini native path drops it (a writer bug), so `thinking` is reported only where the wire can carry it. | `output` − reasoning; if reasoning exceeds output, the provider reported it exclusively and both are kept (drift). Request scope when `api_call_count` = 1, otherwise aggregate with `requests` = `api_call_count`. `route` = `billing_provider`. Billed only when `cost_status = 'actual'` on a single-call row; a mixed row's partial `actual_cost_usd` is never a billed amount. | Reasoning is no longer counted twice. A session whose `model_config` names a Codex thread (`/codex-runtime`) drops its usage with a notice: the Codex adapter counts every request of that thread from the rollout Hermes leaves in `~/.codex`. The 0.21.4 tool names (execute_code, read_file, search_files, skill_view) map into the vocabulary; a session the agent prunes (auto-prune, 90 days) keeps its last usage in the projection. | A multi-call row is estimated as `api_call_count` equal requests and dated at `last_seen`, so a price change or DeepSeek window inside a long row reprices every earlier call and all spend sits in the last hour. Aux calls (title, compression, vision) are normalised inconsistently by the writer. Fast/priority tier and the 1-hour cache TTL are never persisted. Re-pin to 0.21.x pending. |
| OpenCode 1.18.22 (2.x from writer source 2.0.14) | 1.x: `step-finish` parts, one per step, which is one model request (`session/processor.ts:446-455`); `message.tokens` is overwritten per step (`:444-445`). 2.x (2.0.0, 2026-09-11) writes `session_v2` and one `session_message` row per message in the same `opencode.db`; each assistant row is one step with exclusive tokens (`session/usage.ts:11-19`), a compaction row carries its own request, and title generation is recorded only as a `session.usage.recorded.*` event. | From 1.3.16, `input` excludes cache and `output` excludes `reasoning` (`session/session.ts:338-405`). Earlier writers were inclusive in provider-dependent bands, bisected over tags; the rule is chosen per record from `tokens.total` when it can be (exclusive when the five fields sum to it), else by the band. | Per step-finish (1.x) or per assistant/compaction row (2.x), with the writer-version bands applied. Preview builds `0.0.0-<channel>-<stamp>` band by build date, unparseable versions as exclusive with drift. `route` = the provider segment of `providerID`. Never billed; `cost` is a models.dev estimate that also absorbs Copilot's nano-AIU (`llm/ai-sdk.ts:31-43`). | Steps are summed instead of keeping the message's last step. A fork copy is a message created before its session (`message.time.created < session.time_created`), never a step-finish stamped after completion: sessions imported from the pre-1.2 JSON store keep all their usage, and a step-finish dated after its message prices at the message's completion. `agent`, `parentID` and `mode` are optional. Non-default channels (`opencode-<channel>.db`) are read; the pre-split `opencode-next.db` is inventory only because its sessions already live in `opencode.db`. 2.x sessions, forks (`fork_session_id`) and usage are read beside the 1.x tables, and a 1.x session imported into 2.x counts once. | For Anthropic up to 1.18.25, thinking sits inside output and cannot be recovered. Zen resells at its own prices and `big-pickle` has no list price. Native 2.x captures (assistant/compaction rows, an imported session, a fork) are pending. |
| Pi 0.84.3 (usage rules checked against pi-mono a8ed497, 0.87.x) | Assistant `usage`, one per request (`packages/ai/src/types.ts:396-417`). Tool results, compactions and branch summaries also carry usage. From 0.86 a `usage` entry (`session-manager.ts:80-89`) records each prompt-cache warming request (`cache-warmer.ts:331-349`; on by default, `settings-manager.ts:157`); unknown kinds are still usage (docs/session-format.md). | `input` excludes cache on every API. `cacheWrite` includes `cacheWrite1h`. `output` includes `reasoning`. Every API starts a response as all-zero usage without a `reasoning` key and overwrites it only from a usage chunk (`openai-completions.ts:314-321`, `:1546`). | Subsets split and clamped. `route` = the leading segment of `provider` (custom ids are free-form, `model-config.ts:243`); model = `responseModel`, else `model`. `toolResult.usage` becomes an aggregate sample whose model is unrecorded, so it is unpriced. A `usage` entry becomes a request sample keyed by its entry id; a kind other than `cache_warm` is an uncounted aggregate. A compaction is one request, or an aggregate of 2 when the cut split a turn that has history (`compaction.ts:1016-1076`; turn starts `:388-401`; the boundary is the previous compaction's first kept entry). Compaction and branch-summary rows take the latest model in file order (`agent-session.ts:2414`, `:3675`) and none when `fromHook`. An all-zero usage without a `reasoning` key is unreported, never $0. A missing usage count no longer drops the turn. | A forked file's entries older than its header are the parent's copies (`session-manager.ts:1812-1862`): rows stay, usage (including `usage` entries) is dropped; the committed `fork` fixture proves it. `tests/usage_cases.rs` pins the 0.87.x rules with synthetic lines; native captures (cache-warm entries, split-turn and retain-none compactions, `fromHook`, resume with `--model`) are pending the re-pin. | `/import` byte copies keep the same header id, which needs session-level dedupe. Opt-in 1-hour retention that an API does not report as `cacheWrite1h` prices at the 5-minute rate. Pi's own `cost` is its price-table estimate, never billing, and is never read. `context_edit` entries (0.87) and `textSignature` on text blocks drift until the re-pin. |
| Antigravity 1.1.20 (descriptors from the 1.2.9 binary) | The JSONL transcripts carry no usage. Each `conversations/<id>.db` is a `SqliteSnapshot`: every cascade step's `metadata` blob (`exa.cortex_pb.CortexStepMetadata`, field 9 `model_usage`) is the `ModelUsageStats` of that step's own model request, and each `gen_metadata` blob (`CortexStepGeneratorMetadata`) names the steps it wrote plus the served model string (`response_model`); the committed 1.1.20 fixtures carry 5 generation and 3 checkpoint usage records. | `input_tokens` excludes cache reads (cache_read > input in 38 of 40 local records with hits). `output_tokens` = `thinking_output_tokens` + `response_output_tokens` (47 of 47). `cache_write_tokens` is deprecated; `service_tier` is a string, empty locally. | One request sample per step with usage, keyed by the step index, dated by the step's own `created_at`, attributed to the model of the generation that owns the step: `input`, `cache_read`, `output` = the response part, `thinking` = the reasoning part. A step no generation claims (the checkpoint summarizer's calls, whose only model is a placeholder enum) keeps its tokens with no model and stays unpriced. Parts that do not add up are clamped and reported as drift. | 47 requests, 1.42M tokens, $0.31 at list on James's three stores; summing the steps reproduces every generation's total. The protobuf wire is decoded by a small adapter-local reader (varints, length-delimited, fixed fields; unknown fields skipped). | The IDE's conversation `.pb` files are encrypted. Whether background calls (intent, policy guardian, fast-apply, grounding) are billed but unrecorded, and whether subagent stores duplicate usage into their parents. Gemini's Priority/Batch/Flex tiers are now priced; the 2027-01-01 Gemini Flash price step is the catalog's only future period. Native 1.2.9 capture pending. |

**Cross-file usage reconciliation (implemented 2026-09-24 for copies).** Three double
counts cannot be seen within one file:

- Claude Code responses copied into other transcripts: the adapter stamps every usage op
  of a response with `<message.id>:<requestId>`, and the store's process-local owner index
  keeps the earliest-started session's copy, dropping or retracting the others'
  (ARCHITECTURE §4.1.3, counting rule 2; 3,180 responses, 10,761 copies, $1,609 locally).
  Nothing about copies persists (iron rule 5); a daemon opening a complete projection
  starts with an empty index and catches such copies at the next rebuild.
- Codex guardian usage that belongs to its parent: attributed through the review's own
  `parent_thread_id` as `session_usage` samples (Codex row).
- Grok parent turns that fold in child spend, and forked `updates.jsonl` files: still open;
  the fold is a sum, not a copied record, so it needs its own rule.

**Real-ledger check (2026-09-22).** The 2026-09-11 raw ledger was rebuilt with these
adapters, with zero hard errors. Against that ledger's earlier projection, the per-agent
token totals moved as follows (numbers only):

| agent | change | why |
|---|---|---|
| Grok | −48.2% | inclusive `inputTokens` no longer counts cache reads twice |
| Copilot | −49.7% | inclusive input, per-model shutdown intervals |
| Codex | +3.6% | net: usage aimed at dropped rows and polls now lands; replayed subagent prefixes, counter-reset overcounts and reasoning no longer count twice |
| Kimi | +0.7% | session-scope compaction usage |
| Claude Code | −0.04% | net, mostly thinking counted once |
| Hermes, Pi, OpenCode | −0.7% to 0 | exclusive buckets |

What stayed unpriced was what the catalog could not price:

- `grok-*-build`. They are now aliased to grok-4.x (see the Grok row above), and multi-call
  turns price as `modelCalls` equal requests;
- `gpt-5.3-codex-spark`, 0.18M tokens;
- OpenCode's free `big-pickle`;
- one Hermes aggregate above gpt-5.4's 272K tier, now priced by its `api_call_count`.

Rebuilding the same ledger at schema 23 (the aliases and request counts, 2026-09-23) prices
every Grok and Hermes token: all 659 multi-call Grok turns carry their counts, 13,150 model calls.
Only models without an official price remain unpriced: codex-spark and big-pickle.

At that date Claude Code and Kimi priced completely (Opus 5.5 had not shipped). Copilot's one
shutdown interval priced at exactly its billed nano-AIU amount ($0.1865338, gpt-5.6-terra's
period from 2026-07-30); since 2026-09-24 the same amount comes from the five metered
`assistant_usage_events` rows. That is an independent check of the calculator and its dated
periods against a provider's own bill.

**Real-ledger check (2026-09-24, schema 24).** The 2026-09-23 audit ledger (nine agents, 2.93M
records) was rebuilt with this section's adapters, with zero hard errors, and compared with the
schema-23 projection the audit read. List-price totals moved from $79,656 to $80,131 (+0.6%):

| agent | list USD before → after | what moved |
|---|---|---|
| Claude Code | 58,745 → 58,755 (tokens −2.3%) | Opus 5.5 priced (+$1,060). Responses copied across files count once: 1,712 copied responses carried usage on more than one copy, now none. Placeholder-only responses hold no output; 32,033 records are flagged `unrecorded_output`. Usage rides its response row (54.6B of 54.7B tokens on rows, against 27.5B before), so reply heads show it. |
| Codex | 18,574 → 18,978 (+2.2%) | The configured tier back-applied to a session's first turn; 92 remote compactions read from `token_usage_record`; 3,592 guardian requests (325.4M tokens) reach 129 parent sessions as `session_usage`, unpriced because `codex-auto-review` has no list price; two aggregates above the 272K tier with no request count are unpriced instead of mis-tiered (868K tokens). |
| Grok | 2,316 → 2,378 (+2.7%) | `grok-4.7-build` priced: $61.97 at list against $20.93 billed. |
| Hermes, Kimi Code, Pi | unchanged | Kimi's compaction usage moved from samples onto its rows. |
| OpenCode | unpriced | `big-pickle` has no list price. |
| Copilot, Antigravity | absent from the rebuild | Their usage now comes from SQLite streams the audit ledger never captured (`session-store.db`, `conversations/<id>.db`). A fresh capture with the same binary prices Copilot at exactly its billed $0.1865338 from five metered rows, and Antigravity at $0.31 (47 requests, 1.42M tokens; 227 tokens of placeholder-model steps unpriced). An existing ledger gains those streams at its next live run. |

Still unpriced, by decision: `codex-auto-review`, `gpt-5.3-codex-spark`, `big-pickle`, and
aggregates whose tier cannot be decided. Cross-file copies are dropped only when their owner is
projected in the same run; a daemon that opens a complete projection catches the rest at its
next rebuild (ARCHITECTURE §4.1.3, rule 2).

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
