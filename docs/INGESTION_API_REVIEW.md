# Ingestion API: desktop and Vibefield integration review

Reviewed 2026-10-05. **Verdict: the cold-run/live-controller split fits, but the
original proposal was not ready to implement unchanged.** The findings below
describe gaps in that proposed integration, not newly introduced runtime bugs.
The [revised contract](INGESTION_API_PROPOSAL.md) incorporates the required changes.
The follow-up [daemon lifecycle design](DAEMON_LIFECYCLE_PROPOSAL.md) specifies
supervision, platform shutdown, verified exit, and safe connection replacement.

## Scope and actual consumers

- Rosetta working checkout: `21c8bc2a674f7c9528d542055de5530b6348e369`.
- Vibefield checkout: `d9a03ea6590d8cb02191fb226b50caf324f0ef3a`.
- Vibefield's actual Rosetta dependency: dedicated `../rosetta-pin` checkout at
  `856bd801678fe1cc6655229ee2221851995ca368`, protocol 26, recorded in
  [`siblings.lock.json`](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/siblings.lock.json).
  It does not consume edits from the working Rosetta checkout.

The standalone app's Electron main owns its daemon. Its renderer uses one
`RosettaClient`, wrapped by catalog/session/asset ports for `@vibefield/rosetta-room`.
Vibefield's fieldd owns the daemon, enrollment record, bearer, and grant key.
Its History catalog uses fieldd RPCs; transcript bodies use a separate direct
connection carrying a per-session grant. The shared room owns neither transport
nor daemon supervision.

```text
Standalone: History room → host ports → RosettaClient → rosettad

Vibefield:  History room → catalog/control ports → fieldd RPC → bearer → rosettad
            History room → session port → session grant ──────────────→ rosettad
```

## Findings and required corrections

### P1 — Published-history redo must preserve the archive

The original restart performed a fresh scan of currently enabled native roots,
allowing history absent from those roots to disappear from the active projection.
Vibefield explicitly promises that retiring a runtime stops watching while its
rows remain; it even preserves persona labels after the profile and root are gone.
Keeping an old ledger somewhere recoverable would not preserve that browsing
contract. Evidence: [retired-runtime contract](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/contracts/src/agent-index.ts#L123),
[retire implementation](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/fieldd/src/agent-index.ts#L762).

**Correction:** redo on an existing published target uses `basis: archive`:
reconstruct retained evidence and catch up authorized sources. `basis: sources`
remains the empty-target path. A source-only replacement that drops archive
history requires a separate destructive-reset design. Gate published-archive
restart until retention and generation replacement tests exist.

### P1 — Controls need the fieldd control plane and an optional room port

A Vibefield session client cannot call daemon-wide lifecycle routes. Its grant
only opens one session; the bearer deliberately never reaches the renderer.
`RoomPorts` currently exposes catalog, sessions, assets, and optional host services,
with no lifecycle-control port. Merely adding SDK methods would leave the controls
unusable in Vibefield. Evidence: [session-port authority](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/field-app/src/history/session-port.ts#L11),
[room ports](https://github.com/vibefield/rosetta/blob/21c8bc2a674f7c9528d542055de5530b6348e369/clients/room/src/ports.ts#L245),
[local index mutation policy](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/contracts/src/methods.ts#L1686).

**Correction:** add optional `RoomPorts.ingestion`. Rosetta's host supplies direct
SDK controls; Vibefield supplies audited local `index.ingestion.*` and
`index.live.*` RPCs under `index.manage`. Observe through the catalog even when
control is absent. Do not widen session grants or silently enable remote control.

Fieldd's existing GET helper cancels non-success bodies and generally returns
retryable `UNAVAILABLE`. That would lose lifecycle conflicts and their current
revision. Its mutation helper must preserve those details and retain the original
operation id across retries. Evidence: [GET transport](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/fieldd/src/agent-index.ts#L1826).

### P1 — A frozen source set conflicts with persona enrollment

The original proposal only allowed a resumed run's enrollment to shrink.
Vibefield publishes its enrollment record and restarts rosettad when an agent is
enabled or a managed root is added/retired. Rosetta's resumed cold loop already
iterates the current consented targets, including a new target without existing
staged segments. A frozen run would omit a newly authorized persona or require
an unnecessary full redo. Evidence: [enrollment restart](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/fieldd/src/agent-index.ts#L873),
[resumed target loop](https://github.com/vibefield/rosetta/blob/21c8bc2a674f7c9528d542055de5530b6348e369/crates/rosettad/src/serve.rs#L1134).

**Correction:** `scope_policy: follow_enrollment`. Keep run identity and pause
intent across a supervisor restart; derive the effective runtime set from the
current authoritative enrollment, increment the control revision on scope change,
and reset the progress epoch. Never treat a staged manifest as renewed consent.

### P1 — Replacement needs generation-aware reads and cache invalidation

Keeping an old SQLite file readable is not sufficient. The room keys project
statistics by catalog object, project ids, and readiness, then refreshes successful
figures every five minutes. A replacement retaining the same ids and
`canonicalReady: true` would not invalidate that cache. Existing session-port
repointing primarily handles a daemon origin change; an in-process replacement
can preserve that origin and its credentials. Evidence:
[project-stat dependencies and refresh](https://github.com/vibefield/rosetta/blob/21c8bc2a674f7c9528d542055de5530b6348e369/clients/room/src/lib/hooks.ts#L210),
[session-client repointing](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/field-app/src/history/session-port.ts#L20),
[fieldd metadata replacement](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/fieldd/src/index-catalog.ts#L481).

**Correction:** carry the published generation through the status/metadata paths,
fieldd catalog, and room adapters. At commit, reset generation-bound figure,
usage, search, and lazy-entry caches and reject late old-generation responses.
Use existing transcript resync mechanisms with the new generation boundary.
Keep replacement metadata private until publication. Preserve browsing against
the serving generation while displaying candidate progress separately.

### P2 — The current status projection cannot render the proposed lifecycle

Fieldd explicitly folds `/api/status` into `IndexStatus`, and the renderer folds
that again into `CatalogStatus`. The current adapter reconstructs legacy backfill
fields, sets project progress to `[]`, search-stage progress to null, and daemon
timing to null/zero. New fields at the Rosetta endpoint alone would not reach the
room. The phase adapter also recognizes a fixed set of legacy phases; substituting
`paused` there would make it return no status. Evidence:
[fieldd status projection](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/fieldd/src/agent-index.ts#L666),
[History status adapter](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/field-app/src/history/doors.ts#L202).

The room currently idles its poller for 30 seconds when canonical data and search
are ready, regardless of a new background replacement. Its cold activity gate is
`phase !== live`, and its explicit paused presentation only recognizes the old
metadata-audit seam. Evidence: [polling](https://github.com/vibefield/rosetta/blob/21c8bc2a674f7c9528d542055de5530b6348e369/clients/room/src/lib/hooks.ts#L131),
[cold activity gate](https://github.com/vibefield/rosetta/blob/21c8bc2a674f7c9528d542055de5530b6348e369/clients/room/src/lib/ingest.ts#L9).

**Correction:** one additive `ingestion` overview in status, carried end to end.
It exposes current run/live state, serving-generation readiness, observation
epochs, implemented capabilities, and a polling hint. Preserve census, restoration,
project, and search-stage observations. Pause/failure is an explicit activity
state; ordinary warm work never reopens the first-import veil. Do not overload
the old phase enum or require each window to combine three independent snapshots.

### P2 — Preserve auto-start and distinguish process restart from redo

Both supervisors already spawn ordinary `serve` with an ephemeral origin and
depend on a structured ready event before ingestion is finished. The standalone
roll call calls `enableRuntimes`; Vibefield writes enrollment and restarts the
process. Neither presently has a second manual “create ingestion” step. Evidence:
[standalone spawn](https://github.com/vibefield/rosetta/blob/21c8bc2a674f7c9528d542055de5530b6348e369/clients/app/electron/daemon-service.mjs#L67),
[roll-call submission](https://github.com/vibefield/rosetta/blob/21c8bc2a674f7c9528d542055de5530b6348e369/clients/app/renderer/src/components/RollCall.tsx#L60),
[Vibefield spawn](https://github.com/vibefield/vibefield/blob/d9a03ea6590d8cb02191fb226b50caf324f0ef3a/packages/fieldd/src/rosettad-supervisor.ts#L770).

**Correction:** the coordinator adopts those automatic starts. Do not add a
duplicate renderer POST start or change either app to manual startup. A paused or
failed ingestion must leave process health ready and the HTTP service available.
Keep `system.index.restart` as process recovery, protected by its existing local
`native.admin` rule; it is not an alias for rebuilding history. New in-process
retry logic must preserve idempotency across the supervisor's origin/token changes.

### P2 — Publication must be a coordinated sibling upgrade

Vibefield loads its pinned client and room, not this checkout, and both supervisors
check the exact protocol version. New Rust/TS types alone do not update fieldd's
independent wire projections, renderer adapters, contracts, fixtures, or installed
binary. The new control port must be optional for read-only hosts and fixtures.

**Correction:** land coordinator/types first, then host RPCs and adapters, then
perform an explicit sibling pin/build/consumer-check upgrade. Feature flags or
capabilities cannot turn a mismatched binary into a compatible one.

## Recommended delivery boundary

Keep the six cold resource routes and three live-controller routes. Their
lifetimes fit the current engine and host boundaries. Release observation and
pause/resume/discard of unfinished work first. Expose published-archive restart
only after archive retention, coherent generation publication, and both apps'
cache/reset behavior pass integration tests.

The revised proposal defines the host mapping, additive status overview,
enrollment policy, archive-preserving redo, and generation invalidation. It does
not claim the backend, RPCs, or UI controls have been implemented.

## Verification performed

- **21 passed:** standalone daemon-service, ingestion-progress, and host-port tests.
  These cover structured readiness, child ownership, origin changes, status polling,
  progress observations, and direct host adaptation.
- **18 passed:** Vibefield `history-doors.test.ts`, using the installed pinned
  Rosetta packages. These cover catalog projection and session/asset access,
  including grant renewal and origin repointing. The existing Vitest config was
  reused through a temporary wrapper with cache disabled/outside the repository.
- Reviewed enrollment, method permissions/audit, status/error translation,
  supervisor startup/restart, warm/cold recovery, and room cache dependencies.
- Rebuilt the documentation: 205 pages, 26 implemented routes, 166 generated
  types, protocol 26; internal links and fragments passed validation. Parsed all
  three JSON examples in the proposal and checked all 19 cited source targets.

These are current-consumer regression checks. The proposed lifecycle has no
implementation to test yet. No live user ingestion was started, paused, discarded,
or rebuilt, and no application/backend source was changed in this review.
