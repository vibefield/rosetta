# Ingestion lifecycle API

**Status: proposal, 2026-10-05; cold pause/resume subset implemented 2026-10-29.**
The current subset is documented in [ARCHITECTURE.md](ARCHITECTURE.md): auto-started
cold runs expose GET, pause, and resume, with their resource at `daemon.ingest.run`.
The full resource/overview, start, discard, restart, and live controller below remain
proposed; do not infer those capabilities from cold pause/resume support.
The wire contract as of
integration on 2026-10-10 is protocol 27. Apart from that implemented subset,
the routes and types below are proposed additions, not callable endpoints or
generated SDK methods.

**Revised after the desktop/Vibefield integration review.** The resource split
fits both hosts, but the original fixed-enrollment and source-only replacement
rules did not. This version follows current enrollment, preserves the published
archive on redo, and requires the host/status integration in §11. See
[the evidence and rollout review](INGESTION_API_REVIEW.md).

The companion [daemon lifecycle design](DAEMON_LIFECYCLE_PROPOSAL.md) defines
process ownership, private shutdown, health, connection identity, and recovery.
Process shutdown preserves the ingestion intent specified here.

## Decision

Use two lifetimes: a finite **cold run** that publishes a projection, and a
persistent **live controller** that keeps that projection up to date. Sections
1–9 define cold runs; §10 defines warm startup and continuous ingestion.

Represent a cold ingestion as an addressable run, with a stable id across
pause, process restart, and resume. A daemon manages one configured projection
and reserves one execution slot for the current run, including while paused or
failed. Cleanup of superseded staging does not own that slot. There is no queue
or general job system.

The live controller normally owns the writer. Admitting a cold run parks it and
reserves that same writer for the run. There are never competing writers.

Resume continues the same run using its sealed raw evidence. Restart creates a
new run: from sources for an empty target, or from retained archive evidence plus
authorized source catch-up for an existing archive. Discard abandons unpublished work. These
operations never depend on the client keeping an HTTP connection open.

## 1. Routes

| Intent | Request | Result |
|---|---|---|
| Start | `POST /api/ingestions` | Create a cold run for an empty target. |
| Progress | `GET /api/ingestions/{id}` | Read state, phase, progress, recovery, and available actions. |
| Pause | `POST /api/ingestions/{id}/pause` | Stop at a recoverable boundary; retain evidence. |
| Resume | `POST /api/ingestions/{id}/resume` | Continue the same run; restore RAM as necessary. |
| Clean up | `POST /api/ingestions/{id}/discard` | Stop and remove only that run's unpublished artifacts. |
| Redo | `POST /api/ingestions/{id}/restart` | Create a new cold run linked by `restart_of`. |

Use action routes for asynchronous transitions. Do not expose a writable `state`
field, a generic `action` body, or ambiguous `force`, `reset`, and `clean` flags.
`discard` retains a small result receipt so completion remains observable; it is
not an HTTP deletion of the resource itself.

Every accepted mutation returns the affected `Ingestion` object, `202 Accepted`,
`Location: /api/ingestions/{id}`, and `Retry-After: 1`. Restart returns the **new**
run and its Location. An already-satisfied action returns `200 OK`. GET returns
`200 OK`. All lifecycle responses use `Cache-Control: no-store`.

Extend `GET /api/status` with one `ingestion` overview (§11), including the current
run, live controller, serving generation, capabilities, and polling hint. Its
`current_run` names the latest accepted run, including a terminal receipt, or the
adopted owner of an existing projection. This replaces the earlier pointer-only
status design: both apps need to render controls and readiness from one coherent
answer. A reconnecting client can then fetch a run's detail by id. Legacy receipt
adoption is described in §9.

### Start

```http
POST /api/ingestions
Content-Type: application/json
X-Rosetta-Index-Id: idx_example
Idempotency-Key: 51f77f15-935c-43c3-b820-5ab5e052f6b4

{"mode":"cold"}
```

The only run mode in this contract is `cold`: build a private canonical generation.
Start on an empty target uses `basis: sources`; published-archive restart uses
`basis: archive`, as defined below.
The daemon resolves all currently enabled runtimes from its existing consent and
enrollment configuration. It returns their ids in the run. No filesystem paths,
credentials, parser choices, or worker settings cross this API.

Enrollment remains a separate concern. No enabled runtimes yields
`409 enrollment_required`. An occupied execution slot yields
`409 ingestion_exists`, with its run id. An existing published target yields
`409 projection_exists`; explicitly
restart its owning run instead. An orphaned ledger or ambiguous artifact set is
a recovery error, never permission to overwrite it. Empty enrolled roots are
valid and can produce an empty successful projection.

The app policy is `scope_policy: follow_enrollment`. The supervisor's current
consent record remains authoritative when it restarts the daemon after a persona
or enrollment change. Recovery keeps the run id and pause intent, re-derives
effective `runtime_ids`, includes newly authorized roots, and removes revoked
roots from new capture. Report excluded evidence-bearing ids in
`excluded_runtime_ids`; the manifest never grants consent by itself.

`enrollment_revision` is an opaque identity of the loaded effective configuration.
A changed scope increments the run's control revision and resets its progress
epoch before work resumes. The loaded scope is stable during one daemon process;
this proposal does not add a second live enrollment writer. If no source remains
authorized during a source-based initial build, wait with `enrollment_required`.
An archive-based rebuild can still retain already captured retired history. A
runtime id cannot silently bind a different source root; an incompatible binding
fails with `source_changed` rather than consuming evidence under a new identity.

### Controls

```http
POST /api/ingestions/ing_example/pause
Content-Type: application/json
X-Rosetta-Index-Id: idx_example
Idempotency-Key: 77e9a008-d177-48c7-ac4b-b75adfdc5a6c

{"expected_revision":7}
```

Resume and discard use the same body shape. Restart additionally requires the
generation the caller intends to replace, or explicit null for an empty target:

```http
POST /api/ingestions/ing_example/restart
Content-Type: application/json
X-Rosetta-Index-Id: idx_example
Idempotency-Key: e422e2c6-3627-40fc-a7da-9e42ab2a70de

{"expected_revision":12,"replace_generation":"gen_example"}
```

Read that generation from `status.ingestion.projection_generation`. A changed target yields
`409 projection_changed`; the server never infers permission to replace a newer
generation. This token identifies a published generation, not each live revision.

## 2. Resource

This is an illustrative complete running response. Ids are opaque; timestamps
are Unix milliseconds. Proposed types will be owned by Rust and generated for TS.

```json
{
  "id": "ing_example",
  "revision": 7,
  "mode": "cold",
  "basis": "sources",
  "replaces_generation": null,
  "scope_policy": "follow_enrollment",
  "enrollment_revision": "enrollment_example",
  "state": "running",
  "phase": "capturing",
  "created_at": 1791223200000,
  "updated_at": 1791223204000,
  "runtime_ids": ["rt_example"],
  "excluded_runtime_ids": [],
  "restart_of": null,
  "restarted_by": null,
  "progress": {
    "epoch": "progress_example",
    "sampled_at": 1791223210000,
    "runtimes": [
      {
        "runtime_id": "rt_example",
        "stage": "capturing",
        "metadata": {"files_scanned": 120, "files_total": 120},
        "files": {"completed": 42, "total": 100},
        "bytes": {"processed": 12000000, "total": 30000000},
        "sessions": {"discovered": 35, "ready": 18},
        "records_processed": 300
      }
    ],
    "projects": [
      {
        "project_id": null,
        "sessions_total": 35,
        "sessions_done": 18,
        "bytes_total": 30000000,
        "bytes_streamed": 12000000
      }
    ]
  },
  "resumable": true,
  "allowed_actions": ["pause", "discard"],
  "result": null,
  "error": null
}
```

`revision` changes on commands, state transitions, and action-eligibility changes,
including entry into publication. It does not change on counter samples.
`updated_at` follows those control changes; `sampled_at` describes observation.
`allowed_actions` is a server-computed hint; the mutation checks again atomically.
`resumable` describes recoverable evidence, not permission to call resume now.

`result` is null until success, then `{ "projection_generation": "gen_…" }`.
`error` is null or `{ "code": "…", "message": "…" }`, with the actionable next
steps in `allowed_actions`. `progress` may be null while reconstructing counters
or reading a terminal receipt after a process restart. Missing observations must
not be rendered as zero work. Historical counter snapshots are not promised.

## 3. State and phase

**State** answers whether the run is executing. **Phase** names its current work.

| State | Meaning | Normally available actions |
|---|---|---|
| `running` | Executing discovery, restoration, capture, or finalization. | pause, discard |
| `pausing` | Pause accepted; workers are draining to a safe boundary. | discard |
| `paused` | No ingestion worker is active; sealed evidence is retained. | resume, discard, restart |
| `succeeded` | The validated canonical generation has been published. | restart |
| `failed` | Work stopped with an explicit error. | resume if recoverable, discard, restart |
| `superseded` | A paused/failed run handed ownership to a restart. | discard |
| `discarding` | Capture stopped or stopping; owned staging is being removed. | discard (retry) |
| `discarded` | Owned unpublished artifacts are gone; receipt remains. | restart |

Actions also depend on the active slot, current enrollment, and generation
ownership. A run already linked to a successor cannot create another one. A
failed discard is `failed` with phase `cleaning_up` and only `discard` available;
it cannot resume capture or release ownership of half-removed artifacts.

Phases are `discovering`, `restoring`, `capturing`, `finalizing`, `publishing`, and
`cleaning_up`. A paused run retains the phase it stopped in. Terminal success,
supersession, and discard use null. Runtime stages are `queued`, `skimming`,
`capturing`, `applying`, and `done`; runtimes may be in different stages at once.

```text
running ──pause──> pausing ──safe stop──> paused ──resume──> running
   │                                      │
   ├──publish──> succeeded                └──restart──> superseded + NEW run
   ├──error────> failed
   └──discard──> discarding ──cleanup──> discarded
```

### Pause and resume

- `202 pausing` means accepted, not stopped. Only `state: paused` proves workers
  have stopped, their recoverable evidence has been flushed, and no new capture
  will occur. In-flight work may complete; no fixed pause latency is promised.
- A pause accepted before the publication fence prevents publication. Once the
  writer enters `publishing`, pause and discard return `409 publication_in_progress`.
  The fence decision and command admission use the same coordinator lock.
- The HTTP service and reads stay up while paused. RAM may be retained in-process;
  recovery never relies on it. Resume may first report `restoring` while replaying
  sealed evidence, then continue with current source inventory and catch-up.
- Paused intent survives process restart. Running intent recovers automatically.
  A recovered `pausing` intent finishes paused, never starts capture by default.
  `failed` remains stopped until an explicit allowed action; no infinite retries.
- Pausing an already paused run and resuming a running run are no-ops. Resume
  while pausing returns `409 invalid_transition`; wait for a settled state.

### Discard: cleanup with a defined ownership boundary

Discard accepts a running, pausing, paused, failed, or superseded unpublished run.
It first prevents publication and joins workers, then removes only the staging
artifacts named by that run's verified ownership record. Read leases are drained
before deleting anything they may reference. A durable discard intent makes
interrupted cleanup finishable on the next start. Resume becomes unavailable as
soon as discard is accepted. Repeating discard is safe.

Discard never deletes native agent files, a published projection, published raw
evidence, shared/adopted assets, or another run's files. It does not traverse an
arbitrary client-supplied path or follow a replaced directory symlink. A run whose
evidence has crossed the publication fence must first be reconciled to a known
publication outcome; a cleanup request cannot guess from a filename.

Discard of a succeeded run returns `409 published_ingestion`. Removing published
history or retained ledger archives needs a separately designed purge operation.
If a first ingestion exposed partial metadata, discarding emits the existing
snapshot/resync barriers so clients remove that partial catalog. With an older
published projection, discard releases writer ownership to the live controller;
it catches up only if its persisted desired state is running (§10).

### Restart: a new run, with explicit replacement

Restart accepts the latest run when paused, failed, discarded, or succeeded,
subject to current ownership and `replace_generation`. An older run returns
`409 not_current_ingestion`, with the latest id. Running work must be paused first. It creates
a new id, sets `new.restart_of = old.id` and `old.restarted_by = new.id`, and records
`replaces_generation`. With an empty target, `basis: sources` takes a fresh census
and does not reuse the abandoned run's capture checkpoints. With a published
target, `basis: archive` rebuilds from retained raw evidence, preserves archived
and retired sessions, and catches up currently authorized native roots using
valid source-generation/cursor rules. It does not reset generation numbers over
an existing ledger. Pure offline ledger-only rebuild remains a separate operation.

Admission atomically transfers the single active slot and records the successor
before acknowledging it. Paused/failed predecessors become `superseded` and lose
resume permission; succeeded/discarded predecessors retain their outcome. Old
unpublished artifacts remain owned by the predecessor until explicitly discarded.
If the new run fails, it is the recoverable current run; the old run does not
silently reactivate. A duplicate restart never creates another successor.

When replacing a published projection, keep it readable until the replacement
validates and commits. Its live writer is suspended for the replacement's lifetime
to preserve the single-writer rule. Thus `canonical_ready: true` can describe the
old serving generation while the replacement is incomplete; it does not promise
fresh capture. A paused or failed replacement keeps that old view readable.

Watch-before-scan and cursor-relative catch-up cover source changes across the
handoff. Previously published history whose native sources are gone or retired
remains browsable after a normal redo, subject to actual canonical source-rewrite
semantics. Absence from the current enabled set is not a deletion instruction.
A source-only replacement that drops archived history is outside this contract;
it needs a separate explicitly destructive reset/purge design. Publication
switches the entire owned generation coherently, resets affected subscriptions,
and starts detached search for the new generation. `succeeded` means canonical
publication, not search readiness or the end of ongoing live ingestion.

## 4. Progress is observation, not a checkpoint

Reuse the existing RAM progress observers; do not introduce a progress journal.
`metadata` reports the bounded census independently of closed-source capture;
inventory-only files need not be capture tasks. `records_processed` supports
restoration observations. Preserve `progress.projects` using the existing
`ProjectBackfill` shape for the shared room's project meters (including its legacy
zero-for-unknown byte total); do not drop these counters in the host projection.
`files.completed` counts sources closed in this attempt (including explicitly
skipped sources); `total` is the current eligible source census, or null if unknown.
`bytes.processed` counts framed payload, excluding audit/prefix rereads; `total`
is a source-size estimate, or null for unknown totals. Neither is whole-job work.
`sessions.ready` counts admitted canonical sessions, not active agent processes.

Counters may be reconstructed or fall after recovery. Change `progress.epoch`
whenever their baseline resets; clients discard cached rates and percentages.
Use a new epoch on every process recovery and whenever a scope/census revision
would make a previous meter incomparable. Clamp estimated byte meters for display;
never infer success from them. Show finalization and publication as named phases.
No global percentage or ETA is promised.

Poll the job using `Retry-After` (one second while executing; a slower client
cadence while paused is fine). A canceled GET or timed-out command does not pause
or cancel ingestion. Polling is sufficient for the first version; no additional
event stream, replay log, or second socket protocol is required.

## 5. Retry and concurrency contract

All mutations require `Idempotency-Key`, an opaque caller-generated UUID. Controls
also require `expected_revision`. All mutations carry `X-Rosetta-Index-Id`, read
from `status.ingestion.index_id`, to bind the command to one logical index.
An authenticated request for a different index returns `409 index_changed`
before consulting receipts or executing work. The SDK preserves this header
across retries; it is a precondition, not a credential or process instance id.
The server serializes commands with writer
ownership, checks the revision, and durably records intent before returning 202.
No strong ETag is fabricated from a control revision while the response's progress
keeps changing.

After authentication and index validation, an accepted matching key is resolved
**before** revision, shutdown-admission, or transition checks. Same key, method,
target, and canonical body return the same
affected run id and its current snapshot without executing again. This includes
restart returning the same successor. Reusing a key with a different request gives
`409 idempotency_conflict`. A stale revision gives `409 revision_conflict`; refetch
and intentionally submit a new command with a new key. Rejected commands have no
effect and do not reserve keys.

Retain accepted keys for at least 24 hours, including across crashes. Persist only
key/request digests and minimal result references, never bearer tokens or response
payloads. Keep terminal receipts for at least that interval; pin the latest run and
any receipt still owning artifacts or the published generation. Bound control
storage by refusing
new admissions with `429 control_capacity` rather than evicting an unexpired key
or silently deleting evidence. After the advertised retry window, clients refetch
status before starting new work; idempotency is not an indefinite history service.
Reserve control capacity for stopping and cleaning up the current run before
admitting new starts, resumes, or restarts.

## 6. Errors and access

Keep Rosetta's error envelope and add typed conflict details:

```json
{
  "error": {
    "code": "revision_conflict",
    "message": "The ingestion changed; refetch before submitting this action.",
    "details": {"ingestion_id": "ing_example", "current_revision": 8}
  }
}
```

| HTTP | Meaning |
|---|---|
| 400 | Malformed body, unknown mode, missing index precondition, idempotency key, or revision. |
| 404 | Unknown run, or an expired receipt with no retained artifacts. |
| 409 | Index identity, enrollment, ownership, generation, idempotency, revision, or transition conflict. |
| 429 | Bounded control storage is full; honor `Retry-After`. |
| 503 | Coordinator is reconciling publication, the process is draining (`shutting_down`), or it otherwise cannot safely admit work. |

These are proposed error-code additions. They do not exist in protocol 27.
Asynchronous worker failure is a successfully read resource with `state: failed`,
not an HTTP 500 from every progress poll. Synchronous internal failures still use
the existing `internal` response and must not imply the command was unaccepted;
retry an uncertain mutation with the same key.

All lifecycle routes require daemon-level access: bearer when configured, existing
open-local behavior otherwise. Session grants cannot read or mutate runs. Apply the
existing Origin guard to every mutation. No new authority is conferred by a job id.

## 7. Persistence and crash safety

Extend the cold-stage control manifest/receipt, outside canonical user data, with
the stable index identity, run id, configuration identities, revision, desired
state, ownership bindings, successor reference, publication intent/result, and
bounded idempotency receipts.
These are correctness controls. They contain no native payloads, normalized ops,
reconciliation history, progress snapshots, or persisted work queue.

Sealed raw evidence and existing cursors remain the recovery authority. A process
exit may lose RAM normalization work and counter observations. Paused intent must
survive independently of that RAM. Existing manifest formats remain readable.

Current fresh publication promotes the ledger before installing SQLite, leaving
the marker recoverable across that gap. **Online replacement needs an additional
crash-safe generation-set publication design**: a recoverable install intent or
atomic manifest selecting an owned ledger/projection/assets set. Replacing those
paths with unrelated renames is insufficient. Reconcile an interrupted publication
under the projection lock before accepting any controls. Search is disposable and
must never serve a result bound to the wrong canonical generation.

## 8. Implementation and acceptance criteria

1. Put one lifecycle coordinator around the existing cold worker. HTTP handlers
   submit commands; they do not own filesystem deletion, parse sources, or spawn
   competing writers. Keep the control listener alive while paused/failed.
2. Add generation ownership, durable pause/discard/restart intent, and publication
   recovery before exposing destructive or replacement actions. Distinguish the
   daemon shutdown signal from an ingestion pause request.
3. Define Rust protocol types, generate TS, add `client.ingestions.start/get/pause/
   resume/discard/restart`, and preserve the SDK's credential and timeout behavior.
   The new namespace and routes remain explicitly proposed until that work lands.
4. Gate release on race/recovery tests: duplicate start/restart after a lost reply;
   stale commands from two clients; pause versus the publish fence; paused restart;
   discard interrupted between deletions; missing/revoked sources; crash after
   ledger promotion and before SQLite install; replacement failure retaining the
   old generation; subscription reset; generation-safe search; grant rejection.
5. Generate the wire golden, reconcile version changes in Rust and TS, and document
   the routes as current only after the implementation and client checks pass.

## 9. Compatibility and scope

Existing `serve` auto-start and `/api/runtimes/enable` behavior must create/adopt a
run through this coordinator, not a second control path. An explicit proposed
`serve --ingestion-start manual` mode binds and discovers without starting work;
enrollment consent then waits for POST start. Auto remains the default. The manual
policy converts recoverable running intent to paused before advertising controls,
so capture waits for explicit resume. Both modes reconcile publication and finish
discard intent; neither resumes persisted paused intent. CLI shutdown remains
shutdown, and a disconnected client never changes the run's desired state.

Both current app supervisors stay on auto. The standalone roll call continues
calling `enableRuntimes`; Vibefield continues publishing its enrollment file and
restarting the daemon. Neither then submits a redundant POST start. A paused or
failed ingestion leaves the listener and structured process `ready` health alive;
it must not trigger the supervisor's crash-restart ladder. Process restart and
ingestion redo are different controls.

Adopt legacy incomplete stages into a run once under the projection lock. For a
complete installation without a receipt, create one synthetic succeeded receipt
bound to the verified active generation, with unknown progress/timestamps left
null. This provides an explicit restart target without pretending a new ingestion
occurred. Timestamp fields are therefore nullable in the final wire type.

Keep `/api/status` and `/api/ingest/boost` working. Live-watcher controls are defined
in §10. Scheduler priority, enrollment changes, published-history purge, ledger
compaction, and ledger-only rebuild remain separate operations with their own
data-retention rules.

## 10. Warm ingestion: the live controller

Warm startup opens an existing complete projection, reconciles changes since its
committed cursors, and continues watching. It updates the published projection in
normal transactions. It does not build a new cold generation or eventually become
`succeeded`: a quiet watcher is still running.

Keep finite cold runs at `/api/ingestions`. Expose one persistent controller per
configured projection at `/api/ingestion/live`. Its identity survives process
restarts and cold-generation replacement. A live controller has no job id and
does not allocate a new history entry for each watch tick or catch-up pass.

### 10.1 Routes and caller intent

| Intent | Request | Result |
|---|---|---|
| Inspect warm ingestion | `GET /api/ingestion/live` | Read desired/observed state, phase, source health, and observed work. |
| Start or resume | `POST /api/ingestion/live/resume` | Enable continuous capture; reconcile from committed cursors before watching. |
| Pause | `POST /api/ingestion/live/pause` | Stop live capture at a recoverable boundary; keep reads and committed data available. |

Both mutations use `{ "expected_revision": 19 }`, `X-Rosetta-Index-Id`, and
`Idempotency-Key`, with the
same durable admission, replay window, conflict envelope, and access rules as
cold controls. Their Location is `/api/ingestion/live`. A new intent returns 202;
an already-satisfied intent returns 200. Resume of an already running controller
does not trigger another scan. No catch-up request API is included in this first
version; initial activation and every transition out of pause perform one.

There is no warm discard or reset. Normal warm batches are already published and
their raw ledger/cursors are recovery evidence. Pause releases capture resources;
it does not undo commits. Rebuilding the archive uses the cold restart route with
an explicit generation precondition and retains captured history. A source-only
reset is deliberately outside this contract. These choices avoid a `mode: warm` job whose completion or deletion would
have misleading meanings.

### 10.2 Resource and observations

```json
{
  "revision": 19,
  "desired_state": "running",
  "state": "running",
  "phase": "watching",
  "projection_generation": "gen_example",
  "blocked_by": null,
  "observation": {
    "epoch": "live_observation_example",
    "sampled_at": 1791223210000,
    "runtimes": [
      {
        "runtime_id": "rt_example",
        "availability": "available",
        "pending_sources": 0,
        "last_reconciled_at": 1791223209000,
        "last_commit_at": 1791223206000
      }
    ]
  },
  "issues": [],
  "allowed_actions": ["pause"],
  "error": null
}
```

`desired_state` is durable `running | paused`. Observed `state` is
`running | pausing | paused | blocked | failed`. Phases while running are
`restoring | catching_up | watching`; pausing retains the phase being drained,
while paused/blocked/failed use null. Restoring
means recovering transient normalization state or verifying cursor evidence, not
discarding the active canonical projection.

`blocked_by` is null or a reason such as
`{ "code": "cold_ingestion", "ingestion_id": "ing_example" }`,
`{ "code": "projection_required" }`, or `{ "code": "enrollment_required" }`.
If neither a complete projection nor an active cold run exists, resume returns
`409 projection_required`; it does not implicitly start a cold ingest. If a cold
run is present, resume can record running intent while the controller stays
blocked. With no authorized sources, no capture occurs.

The observation is nullable until available and remains RAM-only. Change its
epoch when restarting or replacing the observed generation. Counter/timestamp
observations can reset to null; they are not persisted performance history.

- `pending_sources` is the known queued source count, or null if unknown. Zero
  does not prove that the filesystem has no unobserved changes.
- `last_reconciled_at` is the last completed inventory/stat catch-up for that
  runtime. It describes evidence checked during a bounded pass, not a simultaneous
  snapshot of every source or a full content audit.
- `last_commit_at` measures actual canonical data commits; idle polls and watcher
  heartbeats do not advance it. An old value can simply mean a quiet runtime.
- `availability` is `available | unavailable | unknown`. Missing roots and other
  recoverable per-runtime problems appear in `issues` as
  `{ "runtime_id": "…", "code": "…", "message": "…" }`. They must not make
  every runtime appear caught up; other healthy runtimes can keep watching.
- A fatal shared-store/coordinator error yields `state: failed`, stops capture,
  and exposes an error. It requires explicit resume after recovery is safe.

State, ownership, and generation changes advance the control revision; ordinary
progress samples and watch/catch-up activity do not. `allowed_actions` is derived
from current state and checked atomically by the coordinator. The controller is
the authority; any summary added to `/api/status` is a view of the same state.

### 10.3 Pause, resume, and source fidelity

Pause stops new source admissions, drains or safely abandons in-flight work at
existing ledger/cursor boundaries, and releases capture workers/watchers. A 202
response can still say `pausing`; only `paused` proves live capture is quiescent.
Reads and the existing projection remain usable. Detached search may finish
indexing already committed data; pausing ingestion is not pausing read services.

Resume arms watchers **before** the bounded inventory pass, restores any required
RAM state from raw evidence, and forces cursor-relative reconciliation. Then it
drains hints and enters ordinary watching/polling/auditing. Appends continue from
committed cursors; replacements and truncations use existing source-generation
rules. Do not trust only an in-memory watcher queue after a pause or process exit.

Files may change or disappear while paused. Catch-up processes evidence that still
exists and obeys current consent; it cannot recover uncaptured bytes deleted by
the producer. Existing captured ledger evidence remains intact. Do not promise
lossless collection of transient source data during an intentional pause.

Persist only desired state, control revision, a failure latch with its bounded
error code, and bounded idempotency references in a controller record whose
lifetime is independent of cold-stage manifests. The failure latch prevents
a process restart from bypassing the requirement for explicit recovery/resume;
an accepted resume clears it when the coordinator can safely try recovery.
Running intent recovers through catch-up on daemon restart. Paused intent stays
paused. Preserve the proposed manual-start policy: it starts/restarts live capture
paused until explicit resume. A failed controller stays stopped until a caller
resolves the error and resumes. An idle healthy watcher never becomes completed.

### 10.4 Coordination with cold runs

| Event | Cold run | Live controller |
|---|---|---|
| Normal warm start | No new cold run. | Restore → catch up → watch, if desired state is running. |
| Cold start/restart accepted | Owns the writer after warm work drains safely. | Blocked by that run; desired state is preserved. |
| Cold run paused or failed | Retains its execution reservation. | Remains blocked; the old published projection stays readable when present. |
| Cold publication succeeds | Succeeded with the new generation. | Bind that generation; catch up and watch only if desired state is running. |
| Cold run discarded | Releases its reservation. | Resume against the old generation if enabled; otherwise remain paused or blocked without a projection. |

Record the cold reservation and live-controller ownership change through one
coordinator before acknowledging admission. Cold workers cannot start until the
live writer drains. Live resume never steals the writer from a reserved cold run.
Pausing the live controller while cold work is active records paused intent for
the eventual handoff; it does **not** pause the cold run. Use the run's pause route
for that operation. No accepted pause may be lost during publication or discard.

In the default auto-start policy, a new controller starts with running intent,
so cold success naturally leads to live ingestion. A previously persisted pause
always wins over that default. Warm readiness has no global percent complete:
`canonical_ready` means the projection is readable; source reconciliation and
detached search readiness describe different facts.

### 10.5 Current implementation and acceptance criteria

Today, warm ingestion is already automatic when `serve` opens a complete
projection. `run_warm_live_serve` watches before metadata reconciliation and
`drive_live_runners` forces `LiveRunner::catch_up` before the ordinary loop.
`/api/status` provides cumulative `daemon.runtimes` counters and an ingest phase
of `live`; that phase alone does not distinguish initial catch-up from watching.
The three live-controller endpoints and richer observation above are proposed.

Implement `client.liveIngestion.get/pause/resume` using generated Rust wire types.
In addition to §8, verify pause/restart/resume with missed watcher events, append
and rewrite recovery, unavailable roots, no-enrollment behavior, cold admission
versus a live pause, generation handoff while paused, discarded replacement
respecting desired state, and search continuing safely over committed data.

## 11. Required desktop and Vibefield integration

### 11.1 Host-owned control access

The shared `@vibefield/rosetta-room` consumes injected ports. Add an optional
`RoomPorts.ingestion` control port, using generated lifecycle request/response
types; its absence means observation only. Keep lifecycle observations in the
catalog status, so a read-only host can still explain paused or failed ingestion.
An available control port intersects the server's allowed actions with host
permissions. Label controls by scope: “Pause import”, “Pause live updates”, and
“Rebuild archive”. Do not implement a generic pause-all button as two racing calls.

Rosetta's desktop supplies this port over its owned/direct client. Vibefield
supplies it over audited local `fieldd` RPCs. The proposed mapping is:

| fieldd RPC | Rosetta request |
|---|---|
| `index.status` | GET status, including the ingestion overview |
| `index.ingestion.get` | GET ingestion by id |
| `index.ingestion.start` | POST ingestions (manual hosts only) |
| `index.ingestion.pause/resume/discard/restart` | Corresponding run action |
| `index.live.get/pause/resume` | Corresponding live-controller request |

Declare read methods under local `index.read`; mutations under local
`index.manage`, with required audit like enrollment/retirement. No implicit mesh
fan-out or authority through a session ticket. The daemon bearer stays in fieldd;
per-session renderer clients never acquire lifecycle access. Preserve
`system.index.restart` as the separate existing `native.admin` process recovery
command. It must never be remapped to a history rebuild.

The RPC transports a caller-generated operation id to `Idempotency-Key` without
regenerating it on retries. Preserve expected revision and replacement generation
and the pinned `X-Rosetta-Index-Id` with that original intent. Fieldd captures the
current instance id, origin, and credential as
one daemon-generation pair; after restart it may retry against the replacement
pair for the same logical index. A deliberate user/device/index switch cancels
that pending UI operation instead of retargeting it.

Add a mutation-aware transport helper: retain Rosetta's structured conflict code,
current revision, and relevant run/generation ids in typed RPC errors. Expected
conflicts are non-retryable `PRECONDITION_FAILED` results, not generic retryable
`UNAVAILABLE`. A transport timeout has an unknown outcome: reconcile or retry the
same operation id. Audit acceptance separately from eventual operation success.

### 11.2 One status answer for the room

Add a nullable, additive `StatusResponse.ingestion` overview owned by the
coordinator. Fieldd projects it into optional `IndexStatus.ingestion`; both hosts
map it to optional `CatalogStatus.ingestion`. This does not replace the old
`daemon.ingest`/`IndexStatus.phase` enum during migration.

| Overview field | Consumer contract |
|---|---|
| `index_id` | Opaque stable control-store identity across process restarts; binds UI intent to one configured index. |
| `projection_generation` | Published generation being served, or null before first publication. |
| `catalog_complete` | Completeness of the serving catalog, independently of a replacement's census. |
| `current_run` | Latest run summary: id/revision, state/phase, basis, replaces_generation, enrollment revision, allowed actions, and observed progress; nullable. |
| `live` | The live controller's revision, desired/observed state, phase, blockers, and allowed actions. |
| `capabilities` | Implemented cold controls, live controls, and archive replacement; host policy may reduce these. |
| `poll_after_ms` | Suggested next poll: active/transitioning work stays prompt even when old canonical data and search are already ready. |

Capture control state and generation under the coordinator lock; counters remain
bounded RAM samples with their own observation epoch. Reuse these exact resource
projections for detailed GETs. Do not make each renderer stitch status, a run GET,
and a live GET from potentially different transitions merely to draw one panel.

Update fieldd's explicit projection, its dependency-independent schemas/fixtures,
and both `CatalogStatus` adapters together. Preserve census, restoration, project,
and search-stage observations. Unknown lifecycle capability means unavailable
controls, not an unreachable daemon. Progress epoch and run id, rather than daemon
uptime alone, reset rates/elapsed displays. Polling and mutation completion refresh
the same catalog status; there is no renderer-owned second progress truth.

### 11.3 Publication is an application invalidation boundary

A background replacement does not make the serving catalog incomplete. Keep
metadata, session reads, and aggregates consistently on the old published
generation until commit; candidate metadata must not leak through the public Hub.
Use `catalog_complete` for initial census veils and canonical readiness for
aggregate eligibility. Paused/failed work gets an explicit receipt, not a forever
animating import or a disconnected-service message. Routine warm reconciliation
does not reopen the first-import overlay.

On publication, propagate the new generation through status **and** the metadata
snapshot/reset path that feeds fieldd's catalog and the room. A generation-only
change must be observable even if session/project ids are unchanged. Apply
timeline/collaboration resync barriers and reject stale generation-bound reads.
Invalidate project figures, usage, search, and lazy-entry caches; abort or ignore
in-flight answers from the old generation. Verified content-addressed assets may
be reused by digest. Do not rely on a new daemon origin or credential rotation:
online publication can keep both unchanged.

### 11.4 Rollout and release gates

1. Ship observation, coordinator-owned cold recovery, and optional host control
   ports with generated types. Keep automatic startup and legacy status readers.
2. Ship live and unfinished-cold pause/resume/discard after both hosts carry
   mutations, error details, retry identity, and permission checks correctly.
3. Advertise published-archive restart only after archive retention, coherent
   generation publication, both read planes, and room cache invalidation pass
   end-to-end tests. Until then omit restart from allowed actions for published
   targets and return a typed unsupported-operation error; never emulate it by
   deleting SQLite or clearing the ledger.

The standalone app and Vibefield do not load the same working checkout:
Vibefield consumes the dedicated `rosetta-pin` revision in `siblings.lock.json`.
Deliver a deliberate sibling-pin upgrade with protocol/client/room types,
fieldd schemas and method declarations, host adapters, fixture updates, and both
supervisor handshakes. Current strict protocol checks still apply; feature
capabilities do not bypass a binary/client version mismatch.

Acceptance scenarios must include enrollment changes during a paused cold run,
an archived/retired session surviving redo, live pause during publication, a lost
mutation response across an origin/token rotation, read-only host controls absent,
paused work with the process still ready, replacement while browsing old history,
and unchanged project ids whose figures change on generation switch.

## Source anchors

- `crates/server/src/http.rs`: current routing and enrollment/boost handlers.
- `crates/protocol/src/lib.rs`: current `IngestProgress`, phases, and response types.
- `crates/rosettad/src/serve.rs`: shutdown, recovery, cold execution, and publication.
- `crates/rosettad/src/cold_stage.rs`: owned staged evidence and recovery manifest.
- `crates/rosettad/src/board.rs`: derived, non-durable progress.
- `crates/rosettad/src/main.rs`: explicit private ledger rebuild and writer lock.
- `docs/ARCHITECTURE.md` and `docs/AUTH.md`: persistence and access boundaries.
