# Rosetta Architecture

Rosetta is a multi-agent data ingestion engine, the ground-up rewrite of `p008/spaghetti`.
It has exactly three jobs, in priority order of its consumers:

1. **Search** — full-text search over agent transcript content.
2. **Observe** — full agent metadata + transcripts with live updates, for the agent-view app.
3. **Monitor** — live distilled metric feeds (token activity, tool usage, lifecycle) for
   runtime monitors like `p008/chopsticks`.

The production runtime is Rust end to end; development-time evidence tooling is
standard-library Python. Speed is a feature: cold backfill of a full `~/.claude` in
seconds, file-change → consumer latency in the low tens of milliseconds.

Session recency is `SessionRow.activity`, a nullable projection of recorded
conversation time. User, agent, reasoning, summary, collaboration and tool rows
contribute timestamps; notices, metadata refreshes and standalone usage accounting
do not. Native tool lifecycle timestamps and matched update records contribute to
the resolved tool row. First/last bounds live on message rows so replacing a source
removes its old activity; normal affected-source replay also repairs cross-source
tool joins. Indexed min/max reads include descendants along `parent_session_id`,
and metadata notifications refresh both current and former ancestors when parentage
changes. Old metadata frames cannot assert activity: existing archives recover it
through the same validated raw-ledger rebuild as the rest of the projection.
`started_at`/`updated_at` retain source metadata semantics, while `stream_state`
describes indexing completeness and never asserts that an agent is running.
Archived task/workflow states are likewise last-recorded facts: missing completion
evidence never starts a wall-clock duration or an indefinite activity animation.

> **Implementation checkpoint (2026-08-31).** Nine ground-truth/runtime-adapter packs are
> registered. The generic runtime handles adapter-selected JSONL, bounded document snapshots,
> and SQLite snapshots; virtual membership/deletion, generation reconciliation, deterministic
> checkpoints, polling/audits, and the checksummed raw ledger are implemented. Replacement
> generations and late tool/usage relations stage only in process RAM. A process exit leaves the
> prior projection active and reconstructs the lost accumulator by raw-ledger replay; no normalized
> operation or reconciliation payload is serialized into SQLite. Unknown projection schemas fail
> closed and are replaced only by an explicit rebuild. When a new version admits native metadata
> that an older ledger never captured, one fresh source ingest is required before future ledger-only
> rebuilds become complete again.
>
> Projection schema 14 keeps the one canonical SQLite projection separate from a disposable SQLite
> FTS5 sidecar. `projection_state`, the coalesced `search_dirty` repair set, and their asynchronous
> worker landed together. Full generations publish through an atomic `CURRENT` manifest; epoch,
> revision, acknowledgement, corruption, and crash tests prevent cross-generation results.
> Canonical lists/timelines remain available while search is building. Tantivy 0.26.1 remains an
> optional benchmark-only alternative; SQLite remains the canonical transactional store.
> Verified inline images are immutable, content-addressed members of that canonical projection.
> SQLite carries relocatable service URLs, not Base64; the lossless ledger remains the replay
> source.
>
> **Transcript work-index checkpoint (2026-09-03).** Schema 14 records only source-proven task
> start/terminal transitions and adds a partial `(session_id, ordinal)` index containing only task,
> plan, and artifact messages.
> The Rust `timeline_meta` read composes the complete task board, unique Workflow-run summaries
> with exact current and journaled worker-session references,
> latest openable plans, and deduplicated openable artifacts from those canonical facts in one
> session-scoped boundary. Resumed Workflow attempts collapse to their canonical run id and retain
> the newest attempt as a transcript anchor; the rich run document is not copied into this index.
> The protocol-v14 initial timeline snapshot carries that metadata beside the recent message window;
> relevant commits coalesce to one replacement `timeline_meta_snapshot`. Plan/file bodies remain
> lazy, and an exact session-scoped timeline-row read lands old tasks or Workflow attempts without
> paging through the intervening transcript. This is a read model only: no second
> task/workflow/plan/artifact
> representation is persisted, and the existing messages remain the transcript record.
>
> **Known task-namespace defect (2026-09-04).** Claude's `TaskCreate`, `TaskUpdate`,
> `TaskList`, and `TaskGet` describe shared work items, but `TaskOutput` and `TaskStop`
> address heterogeneous background execution handles. An execution may be a local
> subagent, an in-process Agent Team teammate, or a background shell; it is not thereby
> a task-board item or a workflow. The current Claude adapter classifies the latter two
> tools as `ToolAction::Task`, and the `timeline_meta` query consequently admits stop
> handles as empty task rows. The required boundary is fail-closed: only an explicit
> work-item payload or an existing canonical task row may enter `timeline_meta.tasks`.
> Execution output/stop belongs to process, delegation, or team lifecycle (or bounded
> unknown when its kind is unproven), and cannot synthesize task status or timing. A
> session containing only execution-control events has no Tasks launcher or Tasks tab.
> Session ownership is closed during bounded metadata admission: one normalized primary cwd and
> one optional local-checkout project link per session. Schema 13 uses folder identity by default,
> closes native checkout-group/session-owner hints, and merges only linked worktrees. The stream
> phase cannot manufacture projects or reassign ownership; schema 12 removed the former
> `workspace_obs` relation and `project_assigned_at` arbitration column.
>
> **Fresh-serve checkpoint (2026-08-31; streamed apply 2026-09-27).** The normal first-run app
> path uses the fast parallel cold topology without giving up immediate or progressive reads. The
> listener binds before discovery. Metadata is planned off the writer thread, up to three runtimes
> at a time in registry order (2026-09-27). The writer commits each runtime's bounded source-free
> metadata to a named, shared-memory SQLite database carrying the exact canonical schema, strictly
> in registry order whatever order the plans finish in (runtimes share project rows), and drains it
> into the Hub; projects and top-level sessions therefore appear adapter by adapter. A runtime's
> producer starts the moment its own metadata is public, so Claude Code (second in the registry)
> captures while later runtimes are still skimming. `milestone: metadata-ready` still marks the
> moment every runtime's metadata is public; early runtimes may already be streaming by then, and
> the status phase stays `backfilling` for them (it parks at `metadata_ready` only when nothing
> streams, as in metadata-audit mode). The first runtime that cannot be planned, in registry
> order, fails the start. Adapters capture,
> ledger, and normalize concurrently, each through its own private RAM planner (the cursor CAS
> chain, generations, and restart detection are unchanged; every roster source's cursor is created
> before the walk, in path order in one planner transaction, so each keeps the id creation at its
> turn gave it). Nothing is retained per runtime until
> its capture ends: a producer walks its roster in source-path order and hands each source to the
> sole canonical writer as a *closed source* — terminal generation, final checkpoint, contiguous
> validated ops — as soon as nothing can supersede it. Only a transcript whose session still has an
> uncaptured transcript, or a pending sibling heal after a restart, is held; the heal runs as soon
> as the restarted session's last transcript is captured, so the writer sees exactly one terminal
> generation per source, in path order (the order every order-sensitive upsert assumes). Every
> pass of the walk is a *private build* pass (`IngestOptions::private_build`, 2026-09-28): nothing
> of the source is public yet and publication needs it at a terminal generation, so a generation
> ends wherever the file stops it, by the end-of-file rules — which decide activation and the
> seal, for a replacement generation too. A transcript caught mid-append (its file ends inside a
> record its writer has not finished) ends at its last complete record, the checkpoint keeping
> the unfinished suffix as ordinary append growth for the live runner. A source that vanishes, or
> keeps changing under every retry, after part of a generation was read ends that generation at
> the last record read; one that cannot be read at all closes *skipped* — the writer applies
> nothing for it and its cursor keeps the fresh checkpoint, from which the live runner reads it if
> it returns — and a heal that cannot read its sibling again keeps the generation the sibling was
> held with. An unclean generation activates its parseable rows, a JSONL replacement and any
> snapshot or document generation included. Each such source holds its session `Streaming`, as a
> live pass leaves it. Live and disk-cold passes keep their rules: a mid-append or cut-short
> generation stays staged until a read completes it, and an unclean replacement abandons so the
> published generation survives.
>
> **Fresh-start tolerance (2026-09-28).** Publication stays runtime-granular, but a fresh or
> resumed start no longer refuses a runtime for per-file imperfections the live path already
> absorbs: a file that vanished during metadata; a transcript no skim identifies — counted
> *unfinished* instead when no delimiter ends its first record yet, which makes it not yet a
> session — a source missing or unstable at capture; and malformed records. Withheld files wait
> for the live runner's first-sighting or movement admission; sessions whose sources were cut
> short or unclean publish `Streaming`. A runtime that tolerated anything prints one line,
> `<kind>: tolerated per-file imperfections (vanished=…, unfinished=…, unidentified=…,
> missing=…, unstable=…, hard_errors=…, incomplete=…)`, and the profile counts files vanished
> during metadata, unidentified, unfinished, missing at capture, and unstable under every retry.
> A start still refuses what no later read can repair: a truncated discovery or metadata walk
> (the file cap), a checkpoint conflict, a record past the raw frame limit (§3.5), a resumed
> start's interrupted source that is gone (its reserved generation can never finish), and what
> only a bug or corruption leaves — a closed source with a gap in its operation sequence, an
> undecodable final checkpoint, or no terminal generation. Finite `ingest`, `watch` and the
> disk-progressive engine keep the strict gate that refuses every shortfall. The
> producers do not read their own files (2026-09-27): every runtime registers its fresh JSONL
> sources with one process-wide *capture pool* — one worker per core but one by default
> (`--capture-workers N`; `0` restores the walk in which each producer reads its runtime's files in
> turn) — whose workers
> step each source's pass a tail batch at a time (read, normalize, checkpoint, and the batch's
> finished ledger frames under its roster key) ahead of its producer. The producer becomes the
> runtime's *sequencer*, the only code that touches its planner and ledger, and commits that
> output strictly in path order, so the ledger bytes, the planner's CAS chain and every closed
> source are the serial walk's for any worker count or schedule, and an interrupted build leaves
> the serial walk's prefix. Look-ahead is bounded in bytes of queued output: 512 MiB across all
> runtimes (`--capture-lookahead`), a per-runtime share of it, and 64 MiB for the source a
> sequencer commits next, which workers always serve first; work over its bound parks instead of
> blocking a worker. Snapshot and document groups, sibling heals and non-fresh cursors stay on the
> sequencer at their path position. The 2026-09-27 sweep on the frozen snapshot (23 runs, M1
> Max, 10 cores, host load 40–270, snapshot and ledger on one USB SSD), fitted against host load:
> canonical readiness about 131 s / 195 s serial against 102 s / 170 s pooled at load 100 / 200,
> Claude Code's capture 96 s / 137 s against 63 s / 81 s, at equal CPU and 0.3–0.7 GiB more peak
> footprint. Four to ten workers were not separable within the noise; eight and ten did slightly
> better. Past about four workers that setup turned disk-bound (the one USB SSD carries the
> snapshot and the ledger) and then writer-bound: at 8–10 workers the Codex producer waited up to
> 40 s for the writer's in-flight budget while the writer applied and admitted Claude Code, and the
> pool's workers parked. In that sweep the sole canonical writer was the next wall; the preparer
> below has since taken tool resolution off it. A 256 MiB in-flight budget bounds closed sources
> between producers and the writer. Before a source leaves its producer (its sequencer, when
> pooled), the runtime's `StreamedPreparer` assigns each message its final presentation ordinal
> incrementally under the whole-runtime precompute's key (a session a later
> source repeats a timeline into, or one that receives a source out of path order, is re-ranked by
> path and stored ordinal at admission), and takes each activated source's relational facts —
> tool openers, lifecycle updates, usage, file edits — out of its batch until the session's last
> roster source has been emitted. A session is then *self-contained* unless some source crossed
> it: a row, tool fact, usage or file edit of another session's (or a ledger-only) source that
> names it, or one of its own sources naming another session. A self-contained session's facts
> are folded right there, with the admission fold's own collect-and-finish, so the resolved tool
> rows travel with the source that completed the session; a crossed session gets its withheld
> facts back unresolved and keeps the admission fold, and a source that names an already resolved
> session fails the start closed. The writer applies closed sources while capture continues,
> several queued sources of one runtime per bounded transaction: the same final canonical rows the
> retained path writes, plus the resolved tool rows of the sessions they completed, each stamped
> with its own source's cursor and written once. What is left — usage no resolved row absorbed,
> file edits, and every fact of a crossed session — is collected into that runtime's own RAM fold
> at its original source positions, never another runtime's. When a producer closes, the writer
> admits its runtime: the fold resolves each remaining correlation once and writes each remaining
> tool row once, identity-bearing usage is claimed in runtime tail order exactly as before, then
> one stream-state commit flips every source-backed session out of
> `Pending`. Pending sessions are the admission barrier: rows of an unadmitted runtime may sit in the
> RAM generation, but no timeline, entry, collaboration, plan, file-target or report-work read
> exposes them, their stubs stay out of the metadata mirror, and no row effects are observed.
> Admission remains runtime-granular and in completion order. Watched timelines receive one resync
> barrier and re-snapshot through the ordinary API. No raw record, parser state, candidate value,
> or reconciliation map enters the read target. Finite `ingest` and replay keep the retained
> closed-runtime path (`prepare_captured_runtime` → `apply_prepared_runtime`).
>
> Cold JSONL capture transfers owned normalized batches into the RAM planner instead of
> deep-copying them. Both owned and borrowed entry points enforce the same metadata boundary
> and transactional checkpoint CAS before retaining a batch. The retained path's tool
> reconciliation borrows opener candidates from the retained source buffer while selecting the
> authoritative source position, then clones only the selected row into its retryable plan; the
> source buffer survives until successful commit. Closed source application consumes its batches:
> immediate rows are written first, deferred payloads move into provisional reconciliation deltas,
> and each consumed batch releases its remaining data — as does each admitted plan after its
> commit — on a bounded background reclaimer thread instead of the writer. A usage snapshot reads
> only its response row's id, session, model and timestamp (all stored before `content`, so no
> overflow chain is walked). A closed delta carries no retry
> fingerprint: a closed generation is applied exactly once, a repeated cursor is refused inside its
> transaction and fails its checkpoint CAS in any later one. The fold is two order-independent
> steps shared by both paths — collect (winning opener per correlation, identity-deduped facts
> grouped by correlation, one source-finalization marker per opener, the tail) and finish (resolve,
> order, fold response usage) — so a streamed runtime collects while capture continues and its
> admission moves, rather than copies, what it writes. No raw evidence is dropped.
> Canonical writers and codecs borrow unchanged message/text fields; they copy only for ordinal
> changes, command compaction, or observed live mutations. Source activation searches only that
> cursor's generations, avoiding a scan of the whole runtime buffer for every source. Cold source
> batches coalesce session timestamp maxima before committing; metadata batches retain their
> ordered write semantics. Tool-result activity bounds join the initial tool insert instead of
> causing additional row/index updates. Failed retained reconciliation still keeps the committed
> source buffer for an identical retry.
>
> The owned ledger path groups small records from one source/generation into the existing
> batch-frame format, with at most 1 MiB of extra packing memory. Single or large records
> retain individual frames. The private async writer coalesces intermediate buffer
> flushes until metadata, a source seal or segment rotation; unfinished raw-only generations
> remain ineligible for recovery. Its final joined flush still syncs every segment and
> required directory before publication. Ordinary live checkpoints remain durable per batch.
>
> Fresh and resumed RAM cold builds write the existing plain `RLG1` container, deferring
> compression until canonical publication, initial live catch-up and search readiness.
> One daemon-owned maintenance thread snapshots only segments strictly below each live
> writer's current sequence; active and future segments are ineligible. It verifies the
> original, writes a sibling zstd copy, verifies decoded frame bytes/checksums/digest, syncs,
> and atomically replaces that closed segment. Source/generation metadata, logical offsets,
> frame bytes and the segment index stay valid. Concurrent readers keep their old descriptor
> or open the complete replacement. No normalization or canonical writes run in this worker.
> It targets 10% of one core with 5 ms work quanta and bounded 64 KiB copy/verification reads;
> macOS additionally uses utility QoS and its thread CPU clock, while other platforms
> conservatively pace elapsed active time. Utility priority avoids the indefinite starvation
> observed with strict background QoS on a busy host; measured pacing provides the CPU ceiling.
> Search work takes priority. Shutdown cancels pacing
> and discards any unfinished copy; warm starts rediscover remaining plain segments, without
> a persisted maintenance queue. Live writes and finite CLI ingest retain inline zstd.
> Deferred compression trades additional temporary disk space and a later read/write pass
> for removing compression from cold capture; final ledger durability is never deferred.
>
> `serve --cold-ledger-codec zstd-frames:LEVEL` (the default since 2026-09-28, at level 1;
> `--cold-ledger-codec plain` keeps the deferred pass) moves that compression into the fresh
> build's capture workers instead: each worker compresses its batch's
> finished frames into one independent zstd frame (a trailing seal stays plain), and the
> ledger writes it verbatim into an `RLZ1` segment; the ledger's own small frames — defines,
> metadata, seals, batches under 4 KiB, inline groups — are staged and leave as one zstd
> frame at the next flush or precompressed frame. A batch that reaches the ledger plain (the
> serial walk) is compressed there the same way, so the segments are byte-identical for any
> worker count and decode, segment by segment, to the plain build's bytes; rotation counts
> decoded bytes. The background pass then finds every closed segment already compressed.
> On the frozen corpus per-batch frames reach 5.3× at level 3 (a single stream per segment
> reaches 5.6×; the gap is Claude Code's many one-batch sessions) and 4.7× at level 1, and
> cost 1.45–1.6 (level 3) or 1.0–1.1 (level 1) CPU-ms per decoded MB. A flipped byte tears
> the segment, or — as in the stream container — makes a frame run past the end of the file
> and reads as a truncated tail. The resumed path uses the same codec. Level 1 was chosen as
> the default from interleaved live all-agent runs on a calm host (plain / level 1 / level 3):
> canonical ready 53.3 / 52.1 / 51.7 s on the internal disk, daemon CPU +12.5 s and +20.4 s over
> plain, 17.5 / 6.4 / 5.8 GB written by readiness, and a 15.6 / 4.4 / 3.8 GB ledger, where plain
> then needs about an hour of paced background recompression to reach 3.7 GB; on a USB SSD a
> plain ledger also made capture wait about 30 s for the ledger queue.
>
> After every runtime closes, set-based indexes/rollups, `ANALYZE`, quick/FK checks, and projection
> validation run against that RAM generation. The named `<ledger>.staged` raw generation is then drained and fsynced;
> the canonical database is backed up to a same-filesystem private file, synced, installed by
> atomic rename, and the read router switches RAM → disk before the RAM keeper is released.
> The backup writes that private file without a rollback journal or SQLite syncs of its own
> (an interrupted copy is never installed); the explicit fsync before the rename carries its
> durability.
> Detached search stays explicitly Building and its worker remains paused until this switch; the
> writer wakes it as soon as the published projection is reopened, before any runner's live catch-up.
> The build reads one canonical snapshot and records that snapshot's search revision, so catch-up
> commits landing meanwhile are ordinary revision-stamped repairs of the new generation.
> Watchers were armed before metadata, so source suffixes written during the build feed the live
> runner after handoff. RAM is a temporary location for the one canonical projection, not a third
> representation or a recovery source. The app keeps one HTTP/WS contract across the switch.
> Canonical payload visibility is runtime-granular; exposing a partially reconciled multi-file runtime
> for cosmetically finer progress is forbidden. A future session-partitioned plan must first prove
> that every correlation and replacement boundary is closed inside the partition.
>
> **Cold progress (2026-09-07, protocol v16).** Observation is independent of publication.
> Metadata's atomic session count advances with each admitted identity, before ownership settlement
> publishes the adapter's complete metadata batch. A `ColdProgressObserver` reports framed payload
> at bounded record-batch boundaries; `PayloadBytes` excludes audit, anchor, and prefix rereads.
> Fresh and resumed serve populate a shared RAM `ProgressBoard` from metadata's settled ownership.
> `/api/status` snapshots that board while the writer is busy, and a project's `sessions_done`
> advances only after the complete runtime becomes public. Metadata-only parent placeholders do
> not overcount the source plan. Database/document payloads and resumed suffixes have an unknown
> byte denominator (`bytes_total: 0`); serialized records must never be divided by SQLite file size.
> JSONL byte totals remain source-size estimates, with UI clamping for delimiters and growing files.
> Recovery explicitly reports `restoring`; runtimes distinguish `preparing` and `applying` after
> input reading, and the final canonical phases distinguish `finalizing` from `publishing`.
>
> Detached search reports `SearchState.progress` for its unpublished generation: document counts
> during `indexing`, then `optimizing`, `validating`, and `publishing`. The denominator comes from
> the same canonical snapshot, and its atomic numerator advances every 256 documents. Progress is
> scoped to the projection epoch, reset for retries, and cleared after publication; it cannot leak
> an old full-build observation into ordinary live search repair. These observations persist
> nothing and never change either canonical or search readiness.
>
> The renderer polls status every 500 ms until canonical **and** search readiness are satisfied,
> then returns to 30-second polling (intentional consent/audit pauses use 2.5 seconds). Requests
> are serial, abort after five seconds, retain the last observed progress on failure, and retry
> after one second. A compact panel above the project list describes four user-facing steps:
> discovery, reading, preparation/saving, and search. Step-local meters use real denominators;
> opaque preparation/validation work stays indeterminate. Throughput uses an eight-second window
> of observations, expires after four seconds without movement, and resets on phase changes,
> restarts, or regressing counters. Longer counter pauses distinguish an answering service from
> unchanging work counters. Errors explicitly pause the indicator instead of resetting it.
> Available projects and sessions remain selectable, aggregate figures have a separate
> `statsReady` gate, and completion briefly confirms success before the panel closes. Stage-only
> polite announcements, native disclosure controls, tabular figures, and reduced-motion styles
> keep frequent updates readable without repeatedly announcing changing counts.
>
> **Progress validation.** Campaign `/private/tmp/rosetta-ingest-progress-20260907` retains three
> before/after fresh-serve pairs on the same read-only 200-file, 316,975,655-byte Claude/Codex
> corpus (source manifest SHA-256 `f7c85b319eeb1779a0e29f3dba39bc6be29a221cda09a082109c37b3a3b2fb93`)
> and 12-query ranked-result suite (`e8f727b1d2e12b99e25a145665019c67b5e0eb9c3d0575b64b8acfe51b89d65d`).
> All 17 normalized canonical table digests and all 120,151 search documents/ranked results match
> across six runs. With identical 50 ms diagnostic polling, the new board exposes 40–71 distinct
> project folds and search exposes 52–82 document counts per run; the baseline exposes neither.
> At the renderer's 2 Hz cadence, the short fixture yields 7–10 project observations. Baseline
> byte numerators regress at runtime completion; the new payload numerator has zero regressions.
> Median user CPU is 11.634 → 11.618 s, system CPU 1.968 → 1.993 s, and peak RSS 1,590.3 → 1,600.2
> MiB on the same M1 Max/64 GiB Mac. Wall times vary with shared-machine I/O (median full readiness
> 15.863 → 13.396 s), so this is evidence of low observation overhead, not a throughput speedup
> claim. Binary hashes, commands, full status traces, digests, and timings live in `campaign.json`,
> `serve-results.json`, `progress-analysis.json`, and `verification.json` in that campaign.
>
> The progressive read contract is intentionally asymmetric. Project and top-level-session lists,
> bounded session lookups, and timelines may read the canonical shared-memory generation. Global
> heatmap, usage, and contribution endpoints return HTTP 503 with protocol-v7 `not_ready` until the
> reader switches to the published file; search stays explicitly Building. This is a correctness
> and scheduling boundary, not a loading effect: an earlier renderer launched two aggregate queries
> for each of 135 projects every five seconds, and those shared-cache readers starved the sole cold
> writer. After publication the renderer hydrates project figures with four workers and a quiet
> refresh cadence. Contribution SQL constrains the project session tree before aggregating instead
> of re-grouping the whole corpus for every project.
>
> The app also owns first-run recovery and binary selection. `pnpm dev` builds release `rosettad`
> and passes that exact path to Electron. Electron starts plain `serve`. Before any payload capture,
> a fsynced `.cold-generation.json` inside `<ledger>.staged` binds the projection/ledger names,
> enrollment, and runtime roots. An exit discards only canonical RAM. The next launch reconstructs
> it from sealed staged-ledger generations, treats a raw-only tail as unfinished, reserves that
> generation in RAM, and rereads the source strictly above it; its catch-up passes are private
> build passes like the fresh walk's, and it publishes past the same per-file imperfections. A
> crash after ledger
> promotion but before SQLite installation remains resumable because the marker moves with the
> ledger; successful
> SQLite installation removes it last. `--restart-incomplete` remains an explicit legacy
> archive/restart command, not app startup policy. Recovery never treats RAM as durable state.
>
> A complete projection no longer re-enters the following disk-progressive scheduler. It is marked
> Live before serving, arms watchers before a bounded current-inventory skim, then performs one
> non-blocking queued-hint drain plus forced cursor-relative stat poll. Only changed/new sources
> stream, and the same effect drain feeds HTTP/WebSocket subscribers. The disk-progressive path
> remains for legacy incomplete projection resume and `--stop-after-meta`, but is superseded for a normal fresh app launch. Its listener
> and public projection exist first, consent
> precedes all reads, and every runtime is skimmed before streaming. Cold streaming then runs
> through a four-lane scheduler (live ▸ boost ▸ starter ▸ completion, round-robin across runtimes
> inside the cold lanes). Watcher hints drain into the live lane DURING the backfill, but only to
> reprioritize work inside the metadata cut; once that source's cold task has been consumed, later
> growth belongs to the live runner and cannot recreate a cold obligation. This termination rule
> lets canonical publication complete while agents keep writing. A claimed physical path no
> roster entry covers still re-runs bounded meta so a genuinely new source appears immediately.
> Virtual members of one document/database coalesce into one physical snapshot pass. The 500 ms mirror
> rewarm poll is gone: metadata effects (identity keys only, collected on every path including
> cold generations) are drained by the writer thread between passes into `Hub::absorb_meta`,
> fanning precise `/ws/meta` diffs with no polling, no second read connection pinning the cold
> WAL, and no hub mutation off the writer thread. A refresh naming many sessions (a cold
> runtime's admission names all of them) reads them in one pass: each member's own activity
> bounds once, families rolled up in RAM along the parent links — the same answer as the
> per-session family walk, which smaller refreshes keep. Row effects run SCOPED during cold: each pass
> arms the store with `Hub::watched_sessions` (the open `/ws/session` subscriptions), so an open
> transcript receives its prioritized cold rows — appends push precise rows; a cold seal, retry sweep,
> abandon, or append renumber pushes one `SessionSwept` marker and the subscriber re-snapshots —
> while unwatched sessions pay no mutation log at all (their metadata still flows), and the live
> handoff widens the scope to every session. The cold phase itself is one named unit,
> `rosettad::engine::ColdEngine`: it settles an `IngestPlan` (projects → sessions → sources with
> census bytes, from the skims' committed ownership; unproven ownership stays honestly unassigned;
> a multi-session source is ONE planned pass) and folds pass outcomes through a `ProgressBoard`
> onto `/api/status` as per-project rows (`ingest.projects`, protocol v7) — sessions done/total
> and bytes streamed/total per project, refreshed between passes and kept current through
> mid-cold admissions. Plan, board, and schedule are RAM only, rebuilt from a skim on every run;
> durable progress remains cursors, generations, and `stream_state`. Starter AND completion passes carry a byte-governed slice budget (`--slice-budget`,
> default 1 MiB with a 24-record floor — the measured knee of the 2026-08-31 slice-sweep campaign
> (frozen 9-runtime snapshot, 5 repeats, 32k–2m grid: starter drain ~3–5 s at every budget, full
> cold drain ≈35 s at 256 KiB vs ≈19 s at 1 MiB against an unsliced ≈13 s floor, with 2 MiB
> trading starter latency for little further gain); batches are soft-capped to the slice so a slice is
> ~one transaction): a fresh source's clean prefix ACTIVATES and seals its ledger generation at
> the boundary — indistinguishable from the file ending there and then growing — so every session
> opens readable early while paused remainders requeue as ordinary append growth; app boosts run
> unbudgeted and jump both lanes, preempted by at most one slice, and a session badges Indexed
> only when its last queued task is gone. Two bounds are explicit: snapshot-driver sources
> (SQLite/document) always stream whole because their revision identity is a whole-content hash,
> and a source that turns unclean inside a pass falls back to whole-file handling. Dirty content
> follows live-growth semantics on every cold path (decided 2026-08-31): a dirty FIRST generation
> activates its parseable rows and the session holds Streaming, never Indexed — identical sliced,
> unsliced, or rebuilt, with rebuild electing latest seals for only-dirty paths — while a dirty
> REPLACEMENT still abandons to protect the active clean generation (a private build, with no
> generation published yet, activates it, 2026-09-28), and a raw-incomplete source
> (one holding a record past the capture limit, §3.5) still abandons. An idle daemon appends
> NOTHING to the raw ledger
> (fixed 2026-08-31, found by visual QA): the rotating audit re-reads a quiet file against the
> tail checkpoint's committed content chain (checkpoint v2 — a resumable byte fold over
> `0..committed`; v1 blobs still decode and the first audit establishes their chain in place) and
> escalates to a replacement replay only on a real mismatch — the old audit forced that replay
> unconditionally, re-appending every audited file to the append-only ledger each rotation and
> sweeping its projection. Snapshot databases stat-gate: an unchanged stamp is never even opened
> (a read-only SQLite open writes `-shm` beside the source, the watcher saw the daemon's own
> footsteps, and the resulting hint loop starved the cold lanes — `-shm` is never a hint, and
> poll admissions need evidence: first sighting or movement, never presence). Passes carrying an
> audit budget still hash snapshot content whole, so a stamp-preserving rewrite is caught by the
> audit sweep. A final per-runtime audit pass, `milestone:` stdout lines
> for the budget-sweep harness (search-ready is claimed only when the detached build actually
> reached Ready), and the graceful-stop boundary close the cold phase. Interrupted commits resume
> in place. Finite `ingest` and `rebuild` stay private, validated, atomically installed, and never
> slice. Watchers are armed before scan; the initial search build gets a clean I/O window, then
> the live lane reuses the existing roster rather than re-skimming it.
>
> The accepted six-adapter frozen pair below measured that historical predecessor disk-progressive
> path, not the current normal fresh-app path. It
> used 38,998 matched files, 6,063 skims, 6,419 sessions,
> and 774,219 final messages with zero hard errors. Private atomic wall was 522.54 s. Progressive
> milestones were listener 1.19 s, lists 5.82 s, first readable 13.86 s, canonical ready 538.11 s,
> search ready 704.74 s, and total wall 704.82 s (1.349× private; gate 1.5×). Full SQLite/FK checks,
> exact normalized content-table digests, sidecar cardinality, and 100-cycle ranked-result digests
> matched. The canonical file was 4.691 GB and its detached index 3.430 GB. Diagnostic FTS5
> broad-ranked p95 was 271–380 ms versus selective-ranked p95 26.5–35.4 ms; this supports continued
> Tantivy evaluation but not adoption without relevance and product-mix evidence. Private paths,
> fingerprints, and query text remain unrecorded by policy.
>
> A 2026-08-31 visual product-path diagnostic on the current nine-runtime organic corpus found the
> renderer/shared-cache contention above: the same release/RAM path reached canonical readiness in
> 225.02 s before read isolation. After the gate, bounded hydration, and project-first contribution
> query, metadata-to-canonical took 52.36 s; combining that with the earlier 9.35 s automatic metadata
> phase gives a 61.71 s diagnostic cold-work estimate. The direct visual run was 77.64 s because it
> included manual consent. The final projection passed quick/FK checks with 134 projects, 6,553
> sessions, and 569,004 messages. These are organic measurements, not a frozen before/after pair and
> therefore not merge-admissible performance evidence; they establish the reproduced contention
> mechanism and visual/integrity gate only. Detailed measurements live in
> `docs/COLD_INGEST_IO_FLOOR.md`.

---

## 1. Lessons from spaghetti (what we keep, what we kill)

The autopsy of spaghetti (~151k LOC Rust NAPI crate + ~99k LOC TS + ~15.5k LOC Python)
directly shapes this design.

**Kill:**

| Spaghetti disease | Rosetta cure |
|---|---|
| Three coexisting data models in one 101-table schema (legacy flat, provenance plane, canonical plane) | Exactly two representations: an append-only **raw ledger** and one **canonical projection**. Nothing else, ever. |
| Duplicated parsers in Rust and TS (~20k LOC legacy TS kept as "differential oracle") | Parsers exist **once**, in Rust adapter crates. TS consumers get **generated types** from the Rust source of truth. |
| No migrations — schema version mismatch silently drops the whole DB | The DB is a **disposable projection by design**: on projection-version bump, rebuild from the ledger at disk speed. Losing the DB loses nothing. |
| `projection.rs` at 14,102 lines | Normalization lives in per-agent adapter crates; per-domain modules; CI check caps file size. |
| Consumer contract pinned by string (`spaghetti-sdk@0.7.0-local+rfc012cd`, `link:` dep) — breaks silently | A versioned wire **protocol** with a handshake, semver'd generated client package, and golden contract tests. |
| Schema drift only quarantined into `unknown_native_evidence`, invisible | Drift is a first-class fact: captured per record, surfaced in a `rosetta probe` report and a drift table. |
| Path resolution split between TS (`paths.ts`) and Rust | Agent-specific source knowledge stays in the paired `groundtruth/<agent>` data pack and `crates/adapters/<agent>` parser; the declarative layout is canonicalized once. |
| No network server — NAPI + custom `SPAG` MessagePort framing couples every consumer into the host process | One daemon, localhost HTTP/WS. Any process, any language, no build matrix. |

**Keep (spaghetti's good bones):**

- Watch-before-scan ingestion (subscribe first, then scan — no gaps), ~20 ms event
  coalescing, periodic audit sweep as a safety net.
- Per-object opaque cursors + transactional commits (its `source_objects.committed_cursor`
  / `ingest_commits` pattern was right).
- FTS5 **external-content** tables (index without duplicating text).
- The `observeSession` semantics — epoch / sequence / resync. Chopsticks already speaks
  this shape; rosetta's observe protocol keeps the same concepts so porting is a
  transport swap, not a rewrite.
- Content hashing with blake3, JSONL parsing with sonic-rs.

**The most important recon fact:** chopsticks consumes exactly **one** spaghetti API —
`observeSession`, a store-free live event stream. The 34 query methods and the SQLite
file have no external consumer. Rosetta's service surface is therefore small and
consumer-driven from day one.

---

## 2. The three layers

```
┌─────────────────────────────────────────────────────────────────────┐
│  L3  SERVICE PROVIDER                          crates: protocol,    │
│      REST /api/search, /api/sessions …                server       │
│      WS   /ws/sessions/{id}, /ws/meta  (resumable)                  │
│      WS   dedicated metrics feed                   (target)         │
├─────────────────────────────────────────────────────────────────────┤
│  L2  ADAPTERS + NORMALIZER (the runtime)       crates: core,        │
│      capture: watcher/tailers → raw ledger             capture,     │
│      per-agent parsers (raw types) designed            adapter-*,   │
│        AGAINST the common interface, with the          pipeline,    │
│        L1 ground-truth file as their spec              store        │
│      normalize ──pure fn──▶ canonical facts + analyzers             │
├─────────────────────────────────────────────────────────────────────┤
│  L1  GROUND TRUTH (dev-time evidence pipeline) data: groundtruth/   │
│      fixture farm · mock model · census · schema merge              │
│      layout manifests · facet extractors        tools/groundtruth/  │
│      watch.toml release tracking                                    │
│      PRODUCT: canonical JSON + generated HTML per captured agent    │
└─────────────────────────────────────────────────────────────────────┘
         ▲                    ▲                     ▲
     ~/.claude            ~/.codex               ~/.grok
```

Layer 1 is pure ground truth: a dev-time evidence pipeline whose finished product
is ONE canonical `groundtruth.json` plus its generated `groundtruth.html` report
per captured agent (iron rule #6). It runs no production code. Layer 2 is the
entire runtime: capture, per-agent parsers
written against the canonical model with the L1 file as their specification
(never against fixtures or intuition), normalization, analyzers, storage.
Data flows strictly upward. Consumers never reach past L3 into the store; L3
never parses agent files; L1 never knows what a "session view" is.

---

## 3. Layer 1 — Ground truth evidence

**Sourcing principle:** ground truth is derived from reproducible, authoritative sources
— agent source code, official release channels, and hermetically generated fixtures —
never from whatever agent data happens to sit on a developer's machine. Local data may
corroborate; it is never load-bearing. The per-agent sourcing strategy, fixture-farm
design, and release-watch automation live in
[groundtruth/RESEARCH_PLAN.md](groundtruth/RESEARCH_PLAN.md).

### 3.1 One data pack and one parser crate per agent

`groundtruth/claude-code`, `groundtruth/codex`, `groundtruth/grok` hold evidence only.
Production parsers live separately under `crates/adapters/<agent>`. The pair is the
single home for everything Rosetta knows about an agent:

```
groundtruth/claude-code/
├── watch.toml
├── layout.toml
├── fixtures/<version>/
├── census/
├── sources/
├── groundtruth.json
└── groundtruth.html

crates/adapters/claude-code/
├── src/raw/
├── tests/fixtures_replay.rs
└── Cargo.toml

tools/groundtruth/
├── farm.py
├── mock_model.py
├── census.py
├── model.py
└── render.py
```

### 3.2 The layout manifest

A declarative description of the agent's on-disk footprint — the answer to "what files
are stored in the agent folder":

```toml
format = "rosetta-layout/1"
agent = "claude-code"
data_root = "~/.claude"

[[streams]]
id = "transcript"
glob = "projects/*/*.jsonl"
file_format = "jsonl"
discipline = "append_only"
evidence = "fixture"
```

`layout.toml` is an authoring intermediate. The evidence pipeline folds it into
`groundtruth.json`; runtime parser tests read the canonical JSON, never the TOML
directly, so there is still only one authoritative truth surface.

Spaghetti tracked 16 Claude streams; the manifest is where that inventory lives, is
diffed, and is version-annotated. Any file the watcher sees that matches **no** stream
becomes an `UnknownFile` drift fact — new agent features announce themselves.

### 3.3 Versioned raw schemas

The per-file schemas — "for each file, what is its data schema, tracked by agent
version release":

- Raw types are **serde structs, permissive by policy**: every struct carries
  `#[serde(flatten)] unknown: Map<String, Value>`. A new field in an agent release
  never breaks parsing — it lands in `unknown` and emits a `DriftObserved` fact with
  the field path and a sample. (`deny_unknown_fields` is banned: it turns every agent
  release into an outage.)
- A declared record may intentionally be a canonical no-op because it is bookkeeping or
  duplicates an authoritative rail. That does **not** exempt it from schema checks:
  typed variants retain flatten-unknown paths, while declared raw fallbacks compare their
  keys with the embedded canonical shape. Undeclared variants are soft drift; malformed
  known variants are hard drift.
- Where formats self-stamp, version detection is **per record** — Claude Code transcript
  lines carry a `version` field; Codex rollout files stamp theirs. Where they don't,
  `detect.rs` falls back to structural heuristics. Detected version is recorded on every
  raw record.
- Variant shapes across versions are modeled as enums (`RawTranscriptLine` with
  version-gated variants), not parallel copies of whole modules.
- `groundtruth.json` is **THE canonical ground truth — one file per agent, the
  only source of truth** (`rosetta-groundtruth/1`, implemented by
  `tools/groundtruth/model.py`): stream layout, every record type with its
  strongest evidence class, every field's shape/requiredness/observation count.
  It is rebuilt by `build_canonical` from the intermediate captures under
  `groundtruth/<agent>/sources/` (fixture-derived docs per version, census-derived docs
  per date, SQLite schema dumps) — intermediates contribute, never rule.
  `tools/groundtruth/render.py` writes the committed sibling
  `groundtruth.html`; the JSON diff across a release regeneration is the drift
  review artifact and the HTML is its browsable form.
- `fixtures/<agent-version>/` holds per-version sample corpora, produced by the fixture
  farm (hermetic scripted runs against pinned agent versions — see RESEARCH_PLAN.md),
  redacted and determinized. Every fixture is replayed in CI: parse → normalize →
  golden-file compare. Adding support for a new agent version = regenerate fixtures,
  watch what breaks, extend types, commit new schema export.

### 3.4 Drift operations: `rosetta probe`

A subcommand that scans the live agent directories and reports, without ingesting:
unmatched files, unknown fields per stream (with counts and sample paths), parse
failures, and newly observed agent versions. The same detection runs continuously during
normal ingestion, landing in a `drift_events` table. This is the standing answer to
"keep up with each agent's updates": run probe after an agent updates, refresh fixtures,
extend the raw types.

### 3.5 Runtime handoff: watcher, tailer, and raw ledger

These mechanisms consume the canonical L1 layout but belong to the production
runtime in `crates/capture`; they are described here only to make the evidence →
capture contract explicit.

- **Watcher** (`notify`/kqueue): subscribe first, then initial scan (watch-before-scan),
  with bounded hint coalescing and periodic recovery polls/audits. Every `serve` and
  `watch` runtime shares one coalesced `SourceWake`; the writer waits at most 250 ms
  once per quiet pass, then calls each runner's non-blocking `tick_ready`. An exhausted
  hint budget requests another fair pass immediately. Serve publishes committed effects
  after each runner, before visiting the next. Per-runtime stat polls remain on their
  30-second cadence; a dead watcher requests one rescan and then falls back to polling.
  A poll compares each walked roster file with its cursor's committed evidence — the
  checkpoint's normalizer version and tail-observed length — which the runner holds in RAM
  instead of re-reading and re-decoding every checkpoint (77–94 MB of JSON, mostly adapter
  state, for a large Claude root) on every pass. A fresh start seeds it from the final
  checkpoints the canonical writer committed for each closed source; a warm start, a resumed
  cold build, or any source without an entry reads its cursor row once. The runner is the
  only writer of its runtime's cursors, so it forgets a source's entry just before streaming
  it (hint, poll, admission or audit), whatever that pass commits or rolls back; debug builds
  re-derive every RAM entry from its cursor row on each poll.
- **Layout walks** (2026-09-27): each stream glob is compiled once when its ground truth
  loads, and matching never allocates. A walk never enters a directory below which none of
  the streams it accepts could match (`GroundTruthDoc::may_claim_below` →
  `capture::walk_pruned`): discovery and metadata admission prune with every stream, the
  live poll and `rosettad sources` with driver-backed streams only. Each layout's own globs
  decide what is pruned; shared code names no agent directory. The walk cap still counts
  accepted files only, so accepted files, their order, and truncation are unchanged; a
  pruned directory is never read, so an unreadable directory no stream can claim no longer
  fails a walk. Discovery walks all registered roots concurrently (the first failing root in
  registry order fails the census) and records each file's claiming stream on its
  `ScannedFile`, which the census and metadata admission reuse instead of matching again.
- **Metadata skim** (2026-09-27): sources are read and skimmed by their adapter on a
  bounded pool (at most eight threads, never more than the cores) while one ordered merge
  admits their facts in inventory order, so the metadata batch and its ledger frame are
  exactly a serial skim's; SQLite snapshot sources are still read by the merge itself. A
  session cwd's repository probe runs once per metadata pass, never cached across passes.
- **Tailer**: per-file cursor = byte offset + line count + hash of a head prefix.
  The head hash detects rewrite/truncation (Claude Code rewrites files on compaction);
  on mismatch, the file re-ingests under a new **epoch**. Partial trailing lines are
  buffered, never half-parsed.
- **Record limits** (2026-09-28): the tailer captures any record one raw ledger frame can
  hold (`MAX_RAW_RECORD_BYTES`, 256 MiB less the frame's 20-byte prefix), so every captured
  record is ledgered whole; batches of ordinary records keep their 32 MiB cap, and a larger
  record is a batch of its own. The pipeline normalizes records up to
  `MAX_PROJECTED_RECORD_BYTES` (32 MiB, also the metadata skim's bound on one record and the
  default document limit). A longer JSONL record is quarantined by the pass instead: ledgered
  whole and projected as one `raw_record`/`over_projection_limit` drift fact, like a record
  type the adapter does not project, so its generation stays clean, seals and activates, and
  its session can be Indexed; replay applies the same rule by size, so a rebuild reproduces
  it, and cold, live-append and live first-generation passes agree. Only a record past the
  capture limit is *oversized*: skipped unread, never ledgered, its generation raw-incomplete
  (it abandons, and a fresh start still refuses it). A large record in flight costs about twice
  its size — the tailer's buffer, then its finished frame: one 200 MiB record raised a fresh
  start's peak footprint by about 410 MiB, alike with plain or zstd-frames ledgers and serial or
  pooled capture. The capture pool's look-ahead budgets bound what queues behind it, not the
  step that holds it.
- **Bounded metadata fallback**: the ordinary 64 KiB head + 16 KiB tail skim remains
  the default. An adapter may opt a source into a separately metered scan of complete
  records when one optional fact is still missing; the generic 128 KiB / 256-record
  ceilings remain owned by the pipeline. Claude Code uses this only for a missing main-
  session title; Antigravity uses it only when no complete checkpoint in the ordinary
  windows exposes the exact `USER Objective` section. Complete streaming remains the
  eventual convergence path for both.
- **Snapshot drivers**: `SqliteSnapshot` opens a consistent native database view;
  `DocumentSnapshot` reads one stable, bounded rewrite-in-place JSON, YAML, Markdown, or
  text document. Adapters may lower the pipeline document limit for a small native format
  (Copilot caps `workspace.yaml` at 64 KiB). Both drivers may enumerate multiple virtual
  sessions from one physical source. A member that is rewritten in place swaps generations
  like any replaced source. A member that disappears — the agent pruned it (Hermes 0.21.4
  retains 90 days by default), or the whole store is gone — keeps its last projected
  generation, exactly as a deleted JSONL transcript keeps its rows: an agent's retention
  policy never erases Rosetta's history (product decision, 2026-09-24). Its virtual cursor
  simply sees no further activity.
- **Title reconciliation**: bounded metadata emits a ranked `SessionTitle` fact instead
  of relying on file-walk order. Explicit native labels outrank generated labels, which
  outrank prompt/checkpoint-derived fallbacks; same-rank replacements preserve native
  rename behavior. Streamed title singleton rows use the same precedence and fall back
  to the retained metadata answer after a source-generation sweep.
- **Workspace settlement**: every adapter selects one primary cwd from its session header,
  metadata document, or native session table. Child sessions may inherit a proven parent cwd
  when their own metadata omits it. The meta phase normalizes that path, resolves its repository
  root without invoking Git, journals the resulting `SessionRow` and `ProjectRow`, and only then
  admits the source for streaming. Missing evidence remains `NULL`; transcript cwd changes are
  never a fallback ownership channel.
- **Raw ledger**: every complete raw record is appended with source/generation metadata
  to size-rotated segment files, checksummed with blake3, and durably flushed before the
  corresponding projection transaction. By default `rosettad` stores one runtime
  ledger at `<projection>.ledger/<agent>/`; `--ledger` overrides that root. Segments are
  self-describing and scanned incrementally during rebuild: each defines every source it
  references under a segment-local file id. Live and CLI writers number sources by first
  appearance in the segment; a fresh cold capture installs *roster keys* instead — each
  source's id is its index in the path-ordered roster, in every segment — so a batch's
  frames (one encoder, `ledger/frames.rs`) can be finished before the segment they land in is
  known — which is what lets a capture-pool worker finish a batch's frames before its
  sequencer commits them (`Ledger::append_prebuilt` writes them verbatim, exactly where the
  ledger's own framing would have put them). Readers accept any id and decode the same entries
  either way. The private cold
  ledger's worker is fed through a queue bounded by the bytes it holds (64 MiB), not by
  command count, so a batch's checkpoint and seal commands take no room from the next batch.
  *Correction (2026-09-12):* the per-`<agent>` directory is the DISCOVERED runtime's
  ledger; a supervisor-enrolled runtime of the same kind (§5.2, "Supervisor-enrolled
  runtimes") lives in `<projection>.ledger/<agent>~<digest>/`, and every runtime
  directory carries a `runtime.json` stamp naming its owner. Since 2026-09-04 the default
  container is one zstd stream (magic `RLZ1`); fresh/resumed RAM cold serve instead writes
  `RLZ1` segments of independent per-batch zstd frames (level 1 by default), which every
  reader decodes because a zstd reader concatenates frames, or, with
  `--cold-ledger-codec plain`, plain segments that are compressed after readiness as
  described above. The
  frame stream inside either container is byte-for-byte the
  plain `RLG1` one, per-frame blake3 integrity is unchanged, and readers keep both containers
  forever, so nothing is rebuilt for the format's sake. The stream flushes at every checkpoint
  — a zstd block ends, the frame does not — so the durable prefix stays decodable while the
  match window survives; rotation and close write the epilogue, and a torn or corrupt tail
  reports exactly as a plain one would. Level 3 measured on a real 9.65 GB archive: 5.56×
  overall (claude-code 5.0×, codex 5.6×, grok 8.6×) at 1.0–1.6 GB/s compress and 1.9–3.1 GB/s
  decompress on one core, all on the ledger worker thread that already owns segment IO;
  compressing frames individually would halve the ratio, and lz4 lost on both axes.
  `rosettad ledger compact` rewrites older plain segments in place under the projection lock
  (temp file, sync, decode-and-digest verification, rename; torn segments are left alone).
  Its `--level` stops at 19 and `--long` is explicit because a zstd window is a read-side
  cost as much as a write-side one: every later decoder of a segment allocates its window
  (2 MiB at level 3, 8 MiB at 19, 128 MiB for `--long` or levels 20–22) for as long as the
  segment is open, and a replay opens one per runtime.
  `rosettad ledger verify` is the read-only evidence for comparing two ledgers: per segment
  its container, disk and decoded bytes, clean or torn, and blake3 over the decoded frame
  bytes (container-independent, the digest compaction proves its copies against); per
  runtime an entry digest over a canonical encoding of every decoded entry in visit order,
  which also ignores segment boundaries, file ids and raw-batch packing. Metadata entries
  carry run-dependent values (a runtime row's `discovered_at`), so the entry digest is also
  reported split: a source digest over the raw and seal entries — what two builds of the
  same read-only inputs must share — and a meta digest.
  Each closed segment also appends one line to `<agent>/.segments` naming the sources and
  generation ranges it holds, so a read scoped to one source generation (the transcript's
  exact-command fetch) opens the owning segments instead of walking the ledger; the index is
  derived and rebuildable, and it only ever narrows a read — an unlisted segment is always a
  candidate. Two reasons this exists:
  1. **Agents garbage-collect their own data** (Claude Code's `cleanupPeriodDays`
     deletes old transcripts). Without a ledger, history — and your FTS index over it —
     silently rots.
  2. It is the **replay substrate**: re-running an improved normalizer over all history
     is a ledger scan, not a prayer that source files still exist.

---

## 4. Layer 2 — Normalizer

### 4.1 One canonical model, consumer-driven

> **Status: working hypothesis.** The model below is NOT frozen until the Phase-1
> ground-truth survey covers the full agent roster and the Phase-2 pattern synthesis
> validates these shapes against every agent's reality (§10). Spaghetti's canonical
> model was designed from Claude Code alone and every later adapter had to contort to
> fit it — rosetta does not repeat that.
>
> The current detailed hypothesis — eleven entities, the flattened message-row taxonomy,
> the discover/meta/stream ingest phasing, and the concrete SQLite DDL, checked and amended
> against 9 of 11 ground-truth packs but not yet ratified — lives in
> [`docs/canonical-model.html`](canonical-model.html) (rendered design doc, 2026-08-29).
> Where that file and this section disagree, the HTML is newer.

The canonical model is deliberately small — it covers what the three jobs need, nothing
more:

```rust
enum CanonicalFact {
    SessionUpserted(Session),     // agent kind, primary cwd/project, native id, times, status
    EntryAppended(Entry),         // typed user/agent/reasoning/tool/notice/summary body,
                                  //   parent/causal-group/response linkage, timestamps
    ArtifactUpserted(Artifact),   // todos, plans, tasks, team state
    MetricEmitted(MetricSample),  // e.g. Usage: six exclusive token buckets + pricing facts
                                  //   keyed (session, response_key), canonical flag (§4.1.3)
    LifecycleEvent(Lifecycle),    // session start/end, compaction, subagent spawn/finish
    DriftObserved(Drift),
}
```

### 4.1.1 Strict public message boundary

**Internal session exclusion (2026-09-09, schema 21).** An adapter may mark an
identified source `ledger_only` during bounded metadata normalization. Capture
still records its complete native bytes, but metadata admits no session, project,
or title for it and stream normalization emits no canonical operations. Its nullable
`ingest_cursor.session_id` retains only resumable capture progress; no dummy session,
message, asset, usage rollup, or search repair key is created. The admission bit lives
in the raw ledger's source definition so a fresh-ledger replay cannot recreate a
placeholder. On rebuild, the election pass also asks the current adapter to skim
archived opening records. Only elected generations can exclude a session; the
result suppresses its old metadata/title batches and placeholder creation across
all sibling sources, including empty metadata snapshots. The normalizer-versioned
election cache retains those session ids for parallel replay. Raw source definitions
remain unchanged, while the new projection receives ownerless capture cursors that
agree with the next metadata skim. Codex's internal guardian approval reviews use
this policy; ordinary delegated work and code-review subagents remain visible.
Rescanning an older projection without rebuilding retains its existing cursor/source
binding while capturing subsequent raw records. Reclassification happens only in a
new, validated projection, never as an in-place cursor or session migration.

> **Activation status (2026-08-30): shipped in projection schema 12.** Core types, every captured
> adapter, canonical store/search, protocol, and the client share one closed user-facing timeline
> and fixed tool vocabulary. Native input/result/payload objects remain ledger-only. Correlation,
> evidence ranking, progressive snapshots, and late joins are normalization mechanics, not public
> fields or durable SQLite facts.

A privacy-safe point-in-time audit of the nine installed runtimes classified 377,877 tool
calls. The draft native mapping covered 65.46% overall and 99.65% after excluding Codex's
opaque outer `exec` envelope. Claude, Codex, and Grok carried 2.79 GiB of native tool-result
values, validating aggressive reduction, while ID-less Antigravity calls, progressive Hermes
and Claude snapshots, missing turn/response boundaries, and request fields available only in
later records disproved the draft's two-phase/native-id assumptions. These organic measurements
corroborate the design; refreshed canonical ground truth and hermetic fixtures remain the
activation authority.

The raw ledger is the complete source record. A canonical timeline is instead a
user-facing semantic projection. These are deliberately different jobs:

- `user` means human-authored prose, not merely a native `role: "user"`; tool results,
  developer prompts, environment injections, and transport wrappers cannot become user
  messages.
- `agent` and `summary` contain human-readable prose, never serialized JSON. Reasoning is
  retained only when it is non-empty, plain, and user-visible. Redacted, encrypted,
  signature-only, and empty thinking remains ledger-only; response usage/model/timing facts
  reattach instead of disappearing with a dropped wrapper.
- Lifecycle records become a closed `Notice` union (compaction, truncation, model/mode change,
  permission request/decision, handoff, local-command output, runtime error), not arbitrary system
  text. Adapter normalizer versions invalidate old projections when native transport wrappers gain
  a new semantic projection. Generated/custom titles refine `session.title`, not the timeline.
- Attachments are typed image/document/audio/video/file references, while citations remain
  distinct bounded source/span references. Inline bytes become immutable content-addressed
  projection members; the database retains relocatable refs. Attachment-only prose rows are
  valid and do not fabricate empty text.
- Every entry belongs to a deterministic causal group classified as a human turn, agent cycle,
  parent invocation, system cycle, or synthetic group. Group and optional response derivation
  (`native`, `parent_linked`, `ordered`, or `synthetic`) prevents clients from presenting an
  inferred boundary as exact source evidence.

A tool opener, progressive snapshots, and terminal update fold into one `ToolInteraction`
with a deterministic adapter correlation key, optional native id, namespace/name/provider,
evidence-backed status/timing/failure, and one closed semantic action. There are no
universal `input`, `result`, or `payload` fields. The action families are:

`file_read`, `file_list`, `file_search`, `code_lookup`, `file_change`, `shell`,
`code_execute`, `process`, `orchestration`, `web_search`, `web_navigate`, `web_fetch`,
`delegate`, `task`, `plan`, `collaborate`, `question`, `skill`, `workflow`, `schedule`,
`artifact`, `tool_discovery`, `mode`, `finding`, `notification`, `structured_output`,
`external`, and `unknown`.

Live, progressive, corrected, and out-of-order records use closed request/outcome patches
parallel to this union while normalizing. They have no text/JSON/generic variant and wait only
in bounded process RAM when their opener has not arrived. Missing fields never clear proven
facts. Evidence rank plus source order resolves corrections deterministically; duplicate
snapshots are idempotent. Only the resolved `ToolInteraction` crosses into SQLite. The store
rejects a known-family change, while an `unknown` row may upgrade once when later request
evidence establishes the registry-selected family.

Each family keeps only the data necessary to answer its user-facing question. In
particular: reads keep path/range but not file content; file changes keep path plus exactly
one canonical diff when provable but not whole old/new files, and leave create-versus-update
absent when the source cannot prove it; successful shell calls keep command/cwd/
status but no stdout or stderr; web fetches keep URL/status but no page body. A failed,
denied, interrupted, or timed-out shell call may keep a typed failure and at most 4 KiB / 40
lines of deterministic diagnostic text. User-critical large text (prose, commands, diffs,
collaboration bodies, and structured answers) becomes a content-addressed member rather
than being silently truncated. Members are verified and durably renamed before a
referencing canonical row commits; the detached search worker resolves their full text only
after that commit. They remain part of the disposable canonical projection, not a third
source of truth.

Native parsers stay permissive and drift-aware. An adapter-owned semantic projector may
narrow native values into the common union, but narrowing failure **never** fails the raw
record. Unknown extra fields never erase proven semantic fields. A known typed request
survives an unfamiliar completion: reliable status/timing may still merge, the new outcome
is omitted, and drift is recorded. Unprovable optional facts remain absent. Only an
unregistered namespace/name or an unclassifiable record emits `unknown`. Either case records one aggregated
drift shape plus bounded ledger provenance and stores none of the rejected object. Replaying
the unchanged ledger after adding a projector recovers the richer row.
Registered MCP/plugin projectors may emit at most bounded scalar display fields and resource
references; arbitrary nested JSON cannot cross the boundary.

The projector registry is versioned adapter-owned Rust code, keyed by agent, writer-version
range, namespace, and native name. Each entry has typed request/update functions plus explicit
status, search, and bounds policies. It is neither runtime JSON configuration nor another
ground-truth file. In particular, Codex `exec` becomes an honest orchestration action when no
executed-child trace exists; static JavaScript references can never fabricate child actions.

The detached search projection accepts only five semantic lanes: `prose`, `target` (path,
URL, recipient, resource), `intent` (command, query, objective, subject), `change`
(canonical diff), and low-weight `tool` (canonical kind plus native namespace/name for unknown
actions). A typed exhaustive extractor produces these lanes; the worker never traverses stored
JSON. Read bodies, successful shell output, fetched pages, success
acknowledgements, injected prompts/environment, opaque external values, and empty thinking
are neither stored nor indexed.

**Coverage is prompts and replies (product decision, 2026-09-09).** Only `user` and `agent`
rows become search documents — their prose plus attachment names and citation titles/sources.
Tool activity, thinking, summaries, collaboration traffic, and notices remain canonical and
render in the transcript but are not indexed; the extractor keeps its lanes for them so the
policy is one predicate (`rosetta_store::queries::SEARCH_KINDS`) shared by the document
visitor, the build denominator, and the legacy in-projection path, and the sidecar schema
(4) forces an index built under the broader coverage to rebuild. Measured 2026-09-09 on a
clonefile copy of the organic nine-runtime projection: 584,002 message rows become 151,705 search
documents, the published generation shrinks from 1,043,517,440 to 340,185,088 bytes, and the
forced rebuild reports `building` with live document progress from the first second and reaches
`ready` 35 s after daemon start (indexing ≈13 s, then optimize/validate/publish). Protocol clients receive
generated discriminated unions and render by their tags; key sniffing and best-effort JSON
parsing are forbidden.

The exact per-family keep/drop/search rules, nine-adapter native-name mapping, shared bounds,
target DDL, and activation tests live in
[`docs/canonical-model.html`](canonical-model.html#canonical-message-boundary), “The
canonical message boundary.”

IDs are deterministic: `SessionId = (agent_kind, native_session_id)`,
`EntryId = blake3(session_id, native_uuid_or_line_hash)`. Same input → same IDs, always —
that's what makes replay and dedupe trivial.

The `MetricSample` usage shape intentionally mirrors what chopsticks already reads from
`UsageRevisionV2Fact.buckets` (`input_tokens`, `output_tokens`,
`cache_creation_input_tokens`, `cache_read_input_tokens`).

### 4.1.2 Authored images (2026-09-09)

**Implemented.** The nine-adapter audit and remaining native-capture evidence
work are in [RESEARCH_PLAN.md §5.12](groundtruth/RESEARCH_PLAN.md#512-authored-image-audit-2026-09-09).
The interactive presentation is in
[the transcript record §06A](../clients/app/design/transcript-record.html#prompt-images).
This section owns the attachment contract. Its generated wire types, projection schema,
and affected adapter normalizer versions change together.

The unit remains one authored message: `User { text, attachments }` or
`Agent { text, attachments, citations }`. An image does not create another human
turn, response head, tool interaction, or compaction notice. Attachment-only messages
are valid. Classification happens before media extraction: an image inside a tool
result, delegated message, or injected environment does not become a human upload.

`AttachmentRef` and `ProjectionAsset` provide stored media and three reader-facing
facts: stable attachment identity for live updates; exact links from prose to images;
and a truthful tile when the
record proves an image existed but provides no usable source. The shape is:

```text
AttachmentRef {
  id: string,                       // stable within the owning message
  kind: AttachmentKind,
  label: string?,                   // source-proven reader label, if present
  name: string?,                    // filename; separate from the label
  media_type: string?,
  byte_len: u64?,
  dimensions: Dimensions?,
  message_ranges: TextRange[],       // exact mentions in final canonical text
  source: AttachmentSource
}

AttachmentSource =
  Asset { asset: AssetRef }
  | Reference { reference: ResourceRef }
  | Unavailable { reason: NotRecorded | Omitted | InvalidData | UnsupportedMedia }
```

`Asset` retains today's asset reference. `Reference` retains a bounded recorded
file/URL locator, not bytes and not a promise that today's resource matches history;
an asset cannot be nested inside that variant. `Unavailable` needs no invented URL,
file path, or asset digest. Its reason describes source evidence, never a transient
HTTP failure. Existing kind/metadata validation still applies; all new strings and
collections receive the core's corresponding scalar/id/collection bounds. This
contract retains the PNG/JPEG/GIF/WebP image-member allowlist.

Attachment identity is derived from a stable native part identity, or a deterministic
source-block position when none exists. It is not a content digest: two uses of the
same image keep two attachment slots while sharing one immutable blob. An invalid
or omitted first image retains its slot; a later image never inherits its identity
or label. A late asset can update that same slot only through an exact source-proven
join in the same owning session/message. Joins and candidate payloads stay in RAM,
reconstructed by affected-scope ledger replay.

`label` is presentation metadata, not a parsing rule. The renderer falls back to
“Image 1”, “Image 2”, etc. when labels are absent. `message_ranges` are zero-based,
half-open Unicode-scalar ranges in the **final** canonical text (including text
externalized as `CanonicalText`); they are sorted, non-overlapping, bounded by that
text, and must not conflict across attachments. An adapter emits a range only when
the native representation proves the binding. Claude Code uses the recorded
`imagePasteIds` list in image-block order; Codex uses its native image delimiters.
Missing or ambiguous IDs leave the binding absent. No binding means ordinary text, not
an occurrence-order guess. The Markdown layer uses source positions to decorate
those spans without interpreting vendor syntax, creating nested links, or altering
code spans. A link to an unavailable slot focuses its explanation instead of opening
another image. This has a named consumer: both the transcript and full-message viewer
must open the same attachment from the same mention, including after a search landing.

**Normalization and storage (WebP pipeline, 2026-10-01).** Each native adapter
recognizes its own content blocks, transport delimiters, inline data encodings, and
exact asset joins. The shared adapter-support helper validates Base64/data URLs and
MIME signatures, then calls the vendor-neutral, filesystem-free `rosetta-media` crate.
Images are fully decoded, EXIF-oriented and, for RGB ICC profiles, converted to sRGB
before resizing. Invalid data preserves an unavailable attachment slot. Copilot's
exact asset joins validate the *native* MIME, byte count and digest before conversion;
compressed-byte metadata never replaces that source proof.

One recipe serves photos and screenshots: the Rust `webp` binding to bundled libwebp,
lossy quality 80, method 2, alpha quality 100, sharp YUV off, encoder threading off.
The raw ledger keeps the original native Base64. The image projection stores only
compressed WebP, including the full image: no duplicate original PNG/JPEG is kept.
The full image preserves the complete frame and dimensions up to WebP's 16383-pixel
edge limit; larger edges fit within that limit. No rendition is upscaled. SIMD
Lanczos3 resizing with premultiplied alpha produces these display resources directly
from the same decoded source pixels:

| Resource | Maximum pixels | Geometry | Consumer |
| --- | ---: | --- | --- |
| Rail | 160 on the longest edge | contain | viewer's bottom strip |
| Tile | 320 × 320 | centered cover | transcript row |
| Tile2x | 640 × 640 | centered cover | transcript row above 2× device scale |
| Preview | 1600 on the longest edge | contain | viewer Fit mode |
| Full | 16383 on the longest edge | contain only when required | viewer 1:1 |

Full and preview preserve GIF/APNG/animated WebP frame timing and loop count; tile
resources are static first-frame posters. Limits are 32 MiB encoded input, 32 megapixels
per frame, 64 megapixels across an animation and 256 frames. Oversized/unsupported
inputs retain an unavailable slot; source data stays in the ledger. Two process-wide
jobs bound decode/resize/encode concurrency; identical input bytes coalesce, with a
32 MiB compressed-result LRU and no retained decoded pixel buffers.

Each encoded member has its own BLAKE3 filename and truthful MIME/byte count. The full
`AssetRef.image_variants` map contains four independently content-addressed leaf
references. This avoids key collisions when different originals compress to the same
full image but produce different thumbnails. The store materializes and syncs every
member before publishing its references; identical member bytes deduplicate. No image
table, original-byte cache, Base64 column or normalization journal is added.

The helper never reads paths or fetches URLs. Source-only authored references remain
references; recovering historical bytes requires separately ledgered capture. Exact
canonical tool-file targets use the existing admitted read route with a finite
`?image=rail|tile|tile2x|preview|full&v=1` recipe. Four blocking request lanes share the
two-job codec pool. The mutable-file cache contains only WebPs, keyed by original
content hash; file size/mtime/identity changes invalidate its bounded stat map.
It is limited to 512 MiB/512 source versions, separate from canonical members. Browser
ETag revalidation tracks current files. Browser-supported SVG/AVIF tool files retain
a bounded direct-source fallback, with no original disk cache; SVG responses are
sandboxed. Those uncommon source-only formats receive no generated thumbnails.

Schema 26 / protocol 25 and the seven image-producing normalizer versions land
together. Existing projections follow the established **explicit ledger rebuild**
contract, not an in-place migration: stop the daemon and run
`rosettad rebuild --db PATH` (include `--ledger DIR` if the archive uses a custom root),
then restart. Use the same enrollment file for unstamped enrolled runtimes. The rebuild
validates privately and atomically publishes before pruning unreferenced image members,
including old PNG/JPEG copies. Cleanup follows workflow resources and references inside externalized canonical
documents, and is deferred if those cannot be verified. A failed rebuild leaves the
installed projection and its referenced images intact.

The adapter removes transport text only when structural evidence identifies it;
human-authored quotations of the same syntax survive. Human text blocks retain their
relative order, attachment slots retain their relative order, and the initial UI
presents images in a horizontal strip below the prompt’s text bubble. Agent replies
and full-message views use the same strip beneath their prose. That is an intentional
gallery layout, not a claim to reproduce arbitrary text/image interleaving. The raw
ledger remains available if a future consumer needs an ordered rich-content model.
Activity image references are collected from canonical file reads, artifacts, workflow
resources, and structured/external tool resources. One strip sits outside both activity
fold levels and keeps its DOM identity as the folds open. Stored assets use the usual
asset route; exact canonical tool file targets use the existing bounded read endpoint,
lazily when near the visible area. These previews describe the **current file**, and
the browser loads a sized WebP resource. Unavailable or remote-only
references keep a placeholder. No arbitrary tool output or Markdown URL is parsed
into an image request.

Opening an image snapshots its visible row into the shared file viewer (2026-10-01).
For multi-image rows, a bottom thumbnail strip retains every occurrence in order,
including disabled unavailable slots; repeated asset bytes do not merge identities.
Previous/next buttons and Left/Right keys skip unavailable slots and stop at the row
ends; Home/End select the first/last available image. Switching preserves the modal,
Fit/1:1 and fullscreen state, while revealing only the selected thumbnail horizontally.
Closing restores the original opener without moving the transcript. Exact image
mentions use their owning row, including the outer activity row for folded team/peer
messages; a full-message popup opens its own image row in a nested viewer.
Transcript and gallery images use direct HTTP URLs: 320/640-pixel row tiles,
160-pixel gallery thumbnails, the 1600-pixel Fit preview, and full pixels only on 1:1.
Content-addressed renditions use immutable browser caching. Nearby image elements
mount through continuous intersection observation and unmount offscreen; fixed tile
geometry and attachment occurrence identity remain stable. The renderer holds no
full-image Uint8Array/Blob merely to draw these tiles. The generic worktree/byte-only
viewer retains its 64 MiB dialog-owned fallback cache. Failures keep navigation and
Retry available; closing releases the viewer and visible rail resources. File formats
are not guessed from source filenames in the URL-backed viewer.

This feature adds no OCR, invented captions, or image-content search. Existing
authored prose and attachment filenames remain the search inputs; labels, source
wrappers, encoded bytes, and temporary-path transport are not injected into prose.

**Activation.** The core union, generated protocol bindings, seven affected adapters,
and shared renderer land together with projection/protocol/normalizer version bumps.
The Codex content-block facet is rebuilt from its pinned writer intermediate. The
other six use their existing canonical image fields; isolated native image captures
remain explicit watch gaps. Rebuild from the ledger; do not migrate old rows or repair
them with renderer regexes. Existing
ledgers containing inline media can recover the image without the original temporary
file. Evidence the ledger never captured cannot be recreated.
OpenCode authored prompt identities now derive from the owning native message instead
of a text part, so its old part-based prompt anchors change on replay.

Acceptance requires fixture replay for text+image, image-only, several images,
repeated bytes, missing first image, exact labels, literal markup, malformed encoding,
unsupported media, reference-only content, tool media exclusion, and late/out-of-order
asset joins. Verify deterministic ids and source-generation replacement, asset durability
before publication, ledger-only replay, absence of encoded bytes from timeline/search
JSON, and no new canonical reconciliation state. The renderer must pass compact thumbnail strips and uncropped full-image
viewing at wide/narrow widths, long text with visible attachments, keyboard open/close
and focus restoration, failed-load retry, paging/search landing, light/dark themes, and
stable scroll geometry while images load. Runtime performance claims require the usual
frozen paired campaign; this implementation makes none.

`crates/pipeline/tests/authored_images.rs` exercises durability before publication and
rebuild into fresh assets after deleting the captured native files and original assets.
Adapter conformance tests cover occurrence identity, invalid/missing inputs, exact
bindings, checkpointed joins and replacement snapshots. These synthetic inputs are
tests of the pinned grammar, not substitutes for native fixture captures.
`clients/app/test/prompt-images.html` mounts the production transcript and nested file
viewer with synthetic authored-image and tool-activity cases. Run it from `clients/app` using
`npm run dev:web -- --config test/prompt-images.vite.config.ts`. It includes a one-shot
HTTP failure for Retry, inline and externally stored long-message links, an unavailable
first image and a theme switch. Opening an externally stored message reads its immutable
text member rather than presenting the saved preview as the full message.

### 4.1.3 Usage accounting and list-price USD (2026-09-22)

**Status (2026-09-30): schema 25 / protocol v24 adds reported session totals.** James decided the four
product questions on 2026-09-22: exclusive buckets, the five new usage fields, a list-price
headline, and genai-prices plus an overlay. On 2026-09-23 he set the basis: the figure is an
estimate at each model's official per-token list price, as the provider publishes it and
Artificial Analysis and OpenRouter show it, not a reconstruction of any bill; multi-call
totals are estimated as equal requests, and dated prices stay. A nine-auditor review of every
agent on 2026-09-23 found the arithmetic exact (every record re-prices to the rollup, live
equals cold, every priced record matches genai-prices' own calculator) and the gaps in
coverage and counting; the fixes landed on 2026-09-24 with three more decisions: output an
agent never recorded is stored as unreported and counted (`unrecorded_output`), never guessed;
a session that vanishes from an agent's store keeps its last usage; a free or discounted
serving of a listed model prices at that model's list price. Still open:

- native fixture captures of the mock's `[usage]` scenario, and re-pins of the writers that
  moved during the audit (Kimi Code 2.0.2, Hermes 0.21.4, Pi 0.87.1, Copilot 1.0.88,
  OpenCode 2.x, Codex 0.155);
- Grok parent turns that fold finished children's spend (RESEARCH_PLAN §5.13).

The frozen before/after pair is under **Performance budget** below. Per-agent bucket rules
and their evidence, including rebuilds of the real ledger, are in
[RESEARCH_PLAN.md §5.13](groundtruth/RESEARCH_PLAN.md#513-usage-accounting-audit-2026-09-22).
The concrete DDL is in [`canonical-model.html`](canonical-model.html). This section owns the
usage contract. Claude Code normalizer 32, projection schema 25, protocol v24 and the
generated TypeScript types change together; the price catalog is unchanged.

**What a usage record is.** One usage record describes one model request as the agent
recorded it. It rides on the response's timeline row, on a replacement `Usage` op for that
row, or on a `session_usage` sample when no response row can own it. Examples of the last
case: an invisible response, a per-session total, an extra attempt inside one response.
A timeline row that carries no usage makes no usage statement: re-emitting it (a replayed
record, or a Claude Code row whose usage travels as a `Usage` op behind it) keeps the usage
columns it already has, and usage returns to unknown only through an all-`None` `Usage` op
or the replacement generation that drops the row.
Requests without individual usage remain outside request-level coverage. A cumulative
session total can recover their aggregate usage and estimated cost (below). The product
reports an estimate, never "your bill".

**Buckets are mutually exclusive.** Every token lands in exactly one bucket, so sums never
double count and the existing `SUM` queries remain correct:

| bucket | counts | priced at |
|---|---|---|
| `input` | prompt tokens neither read from nor written to a cache | input rate |
| `cache_read` | prompt tokens served from a cache | cache-read rate |
| `cache_write` | prompt tokens written at the provider's default (≤ 5-minute) cache TTL | cache-write rate |
| `cache_write_1h` | prompt tokens written to a 1-hour cache | 1-hour cache-write rate |
| `output` | generated tokens, excluding reasoning | output rate |
| `thinking` | reasoning/thinking tokens | the model's reasoning rate, else its output rate |

A total is the sum of the six. A request's prompt size, which selects long-context tiers, is
`input + cache_read + cache_write + cache_write_1h`. `None` still means never reported,
never zero. The standards report inclusive totals: OpenTelemetry GenAI and genai-prices
put cache inside input and reasoning inside output. Langfuse and Helicone subtract at
ingestion, and so does Rosetta: each adapter maps its native fields into the exclusive
buckets using its rule in RESEARCH_PLAN §5.13, and a fixture proves the rule. A native subset
larger than its total is malformed: the adapter clamps the remainder at zero and emits drift.

**The five new usage fields** (iron rule 5 product decision, 2026-09-22):

- `cache_write_1h`: the bucket above.
- `web_searches`: server-side web search requests the provider bills per call.
- `service_tier`: closed `standard | priority | flex | batch | fast`. Absent means not
  recorded and prices as standard.
- `route`: the serving provider when the record names one that differs from the agent's
  default, as a short normalized slug (`openrouter`, `amazon-bedrock`), optionally with a
  residency qualifier (`anthropic/us` for Anthropic's US-only inference). Never a URL, key,
  or account id.
- `billed_pico_usd`: the provider's own billed amount for this request, when the provider
  reports one. Examples: xAI `cost_in_usd_ticks`, Copilot nano-credits, an OpenRouter
  `usage.cost` passed through. Amounts an agent computes from its own price table
  (OpenCode's `cost`, Pi's `cost`, Claude Code's cost state) are estimates, not billing.
  Reported session estimates have their own explicit contract below; they never populate
  `billed_pico_usd`.

Every usage record also carries `scope` (`request | aggregate`): whether it is one request
or a total the agent reports over several. Examples of totals are a Grok turn, a Copilot
shutdown interval, and a Hermes per-model session row. Pricing needs it because a long-context
tier can only be decided per request. A message row stores `usage_scope` only when it is an
aggregate, and a `session_usage` sample stores `scope`. An aggregate also carries `requests`,
its request count, when the agent reports one: Grok's `modelCalls`, Copilot's interval
`requests`, Hermes' `api_call_count`, and the number of Claude `message` passes.

**Counting rules for every adapter.**

1. *One record per request.* Cumulative counters become per-request deltas once, in the
   adapter. A counter that goes backwards starts a new epoch and counts its whole reading;
   there is no per-component reset (the pre-2026-09-22 Codex fold re-added a component's
   whole running total after any dip).
2. *Replayed history is not usage.* A row that a child, forked or resumed file copies from
   another session keeps its timeline role and drops its usage. Within one file the adapter
   decides (Codex subagent rollouts before `subagent_history_start_ordinal`, Pi and Kimi
   Code forks, OpenCode fork copies). Across files the adapter stamps every usage op of a
   response with the response's native identity (Claude Code: `message.id:requestId`) and
   the store keeps one owner per identity — the earliest-started session, then the lower
   session id — dropping later copies on arrival and retracting an earlier-applied copy's
   usage when its origin lands (`apply::UsageOwners`). The index is process-local like the
   RAM reconciliation buffer: a rebuild replays the raw ledger through it, and a daemon that
   opens a complete projection starts empty, so a copy whose origin was projected in an
   earlier run is caught at the next rebuild. Copies never persist anything (iron rule 5).
3. *Extra attempts are separate records.* A fallback, compaction or advisor call inside one
   response (Claude `usage.iterations`) becomes its own `session_usage` sample with its own
   model.
4. *Multi-step messages sum their steps* (OpenCode step-finish parts). A message never keeps
   only its last step's counts.
5. *Internal sessions still cost money.* Usage from sessions hidden from the timeline, such as
   Codex guardian reviews, attributes to the parent session as `session_usage` when the parent
   is source-proven. Otherwise it is reported as out of coverage. The store admits only such
   samples and drift from a session-less source; they carry the hidden source's own cursor as
   provenance, so a rewrite of the review retracts them like any other generation.

**Cost is computed in the rollup, not the adapter.** Adapters extract facts only. Because
`normalize()` never sees a price, a price update never invalidates a checkpoint or forces a
ledger replay. The store prices each usage record inside the existing hourly rollup, in both
the live `rebuild_bucket` and the cold set-based fold, through the deterministic SQLite scalar
function `rosetta_list_cost(...)` backed by the `rosetta-pricing` crate. It returns pico-USD,
or NULL when the record cannot be priced. `usage_bucket` gains five columns:

- `cache_w1h_tokens` and `web_searches`, summed like the other components;
- `cost_pusd`: the list-price-equivalent cost of the priced records, NULL when none priced;
- `billed_pusd`: the sum of provider-billed amounts, NULL when none were reported;
- `unpriced_tokens`: tokens in records the catalog could not price.

Any aggregate's coverage follows from those columns. It is complete when `unpriced_tokens = 0`,
partial when priced and unpriced records mix, and unavailable when `cost_pusd` is NULL with
tokens present. The transcript's per-response cost is computed at read time from the stored
facts, at tens of nanoseconds per row; it is never stored per message.

**Reported cumulative totals (James's product decision, 2026-09-30).** Consume Claude Code's
`cost-state` alongside the request facts and use the higher reported/calculated estimate
per model over the session tree. `SessionUsageTotals` replaces the selected per-model
totals in `session_usage_total`; repeated snapshots never become additive samples.
This is the user-visible cumulative reading, not a journal of native snapshots. The
adapter checks the native session identity, ignores copies in child transcripts, and
emits facts without consulting prices. It uses the latest preceding transcript timestamp
(checkpointed), or the native start time, for attribution because the snapshot is undated.

The ordinary `usage_bucket` rows have `source = recorded` and still price each request
independently. After those rows change, the store compares the selected totals with all
requests in the session's subtree and writes only positive differences as `source = reported`
rows on the reporting session. Nested totals reconcile from descendants toward ancestors.
Model aliases resolve through the same catalog identity used for pricing. Cost takes the
maximum per model; input, inclusive output, cache reads, combined cache writes, and search
counts each take their maximum. Thinking fills only the missing inclusive output, never
adds another copy of those tokens. Known 1-hour writes retain their TTL; additional writes
with no TTL use the general cache-write bucket and are not independently re-priced.
The original request facts and per-response costs remain untouched.

Live updates (including later-arriving children), source replacement, cold folding and
catalog re-pricing all rebuild the differences. Provenance retracts a replaced source's
selected total. `reported_cost_adjustment_micro_usd` exposes the added estimate separately
from list-price request cost and provider billing; the app describes both parts. Unknown
prices and missing per-response output retain their coverage markers: a cumulative reading
does not prove every response complete. A stale snapshot is a floor, not an increment to
new requests, so the total may hold at that floor until request accounting exceeds it or
a newer snapshot arrives. Corrections have coarse timestamp attribution; they cannot
reconstruct when the missing requests occurred or their cache TTLs.

**Money is an integer.** Inside Rosetta every amount is integer pico-USD (10⁻¹² USD; Langfuse
stores the same unit, Helicone 10⁻⁹). Catalog rates are integer pico-USD per token, which is
exact for any price with at most six decimals per million tokens. A record's cost is then an
exact integer product, and sums never drift. `i64` pico-USD covers $9.2M per sum. The wire
carries aggregates as integer micro-USD, rounded half-up once at the boundary, which
JavaScript represents exactly up to $9 billion.

**The price catalog.** `crates/pricing` compiles a static table that `tools/pricing`
generates. The source is pydantic/genai-prices v2 data (MIT), vendored at a pinned commit
with its license and SHA-256, plus a repo-owned overlay. The generated table is committed, so
every price change is a reviewable diff.

- *Basis:* the model creator's first-party API list price ("API-equivalent"). Reseller
  tables (OpenRouter, Copilot, Bedrock) never feed the headline figure.
- *Exact matching only:* the pricer removes a trailing date stamp (`-YYYYMMDD`,
  `-YYYY-MM-DD`) and a context tag (`[1m]`), and the result must equal a catalog id or an
  explicit alias. genai-prices' prefix rules are not used; its `starts_with: claude-opus-5`
  would price `claude-opus-5-5` as Opus 5.
- *Dated:* each model carries effective-dated price periods (genai-prices' `start_date` and
  time-of-day windows). A record prices at the period in force at its own UTC timestamp:
  `gpt-5.6-sol` is $5/$30 before 2026-08-21 and $4/$20 from then on.
- *Long-context tiers* reprice the whole request when its prompt size exceeds the tier start.
  The comparison is strictly greater-than, genai-prices' convention, which encodes xAI's
  inclusive 200K boundary as 199,999.
- *Overlay:* cited additions only, in five entry kinds. `[[alias]]` adds an exact spelling
  of a catalog model (`k3` → `kimi-k3`; the dotted `claude-fable-5.1` and `claude-opus-5.5`
  that Copilot, OpenRouter and Hermes record; Kimi Code's plan ids); an alias may re-point an
  upstream spelling only by naming what it displaces. `[[model]]` adds a model genai-prices
  lacks (grok-4.7, and Grok 4.7 Fast as its own price table), with dated periods where the
  price changed. `[[patch]]` corrects or completes an upstream model's rates in every period
  (Moonshot's K3 cache writes), because a same-id `[[model]]` would be shadowed and is
  rejected. `[[tier]]` and `[[route]]` carry multipliers, optionally dated (`from`/`until`),
  and a tier entry may name both spellings a provider accepts. A cache rate is never derived
  from the input rate unless the provider documents the multiplier. Free or discounted
  servings of a listed model alias the listed model — Grok Build's `grok-4.x-build` (xAI's
  charge equals list to the tick until a mid-August 2026 discount), OpenCode Zen's `*-free`
  ids — because the figure is what the usage would cost at list. A model with no published
  list price, such as Zen's stealth `big-pickle` or `codex-auto-review`, stays unpriced, and
  any billed amount is still shown.
- *Multipliers scale tokens only:* service-tier and residency rules multiply the token cost,
  as their sources cite. Per-call search fees and per-request fees stay flat. Anthropic fast
  mode is 2× on Opus 5.5 (from 2026-09-22), Opus 5 and Opus 4.8 (from 2026-05-28), and
  standard on Opus 4.6 from 2026-05-29 (its earlier research-preview premium is uncited, so
  those records stay unpriced); `inference_geo: "us"` is 1.1× on every Claude 4.6+ model as
  the route `anthropic/us` (from 2026-02-05). OpenAI Fast mode (Priority until 2026-07-30,
  both spellings accepted) is per model: 2× on gpt-6-astra/sol/luna, gpt-5.6-sol/terra/luna,
  gpt-5.4, gpt-5.4-mini, gpt-5.2 and gpt-5.3-codex, 2.5× on gpt-5.5; Flex and Batch are 0.5×.
  Gemini's Priority tier is 1.8× and Batch/Flex 0.5× provider-wide. A model-specific rule
  beats a provider-wide one; a dated rule needs the record's timestamp, so an undated record
  that only dated rules could price is ambiguous.
- *Counted aggregates:* an aggregate with a request count is estimated as that many equal
  requests. Each carries the average prompt, which selects the long-context tier, and a
  per-request fee applies once per request. On James's Grok history this estimate is about
  $2.3K, against $1.4K at base rates and $2.9K if the summed prompt picked the tier.
- *Unpriced, never zero:* the causes are an unknown model, no price period at the timestamp,
  an unsupported tier or residency, a non-zero bucket without a rate, and an uncounted
  aggregate whose prompt tokens exceed a tier start. A record without a time is ambiguous on a
  model whose price changed, or when only a dated tier or route rule could apply. Usage ops
  write that "no time" as `ts = 0`, which the store never prices as 1970-01-01. The resolver
  also reads Amazon Bedrock's inference-profile grammar (`us.anthropic.claude-opus-4-6-v1:0`
  is `claude-opus-4-6`) and treats a leading `@` (Cloudflare's `@cf/…`) as a prefix, not a
  version suffix.
- *Unrecorded output:* a streamed response that never got its final usage record has a known
  prompt and an unknown output (Claude Code writes a 1–99-token placeholder on every block
  record and the real figure only on the last; 15.7% of James's responses, 30% in September
  2026, never get it). The adapter stores such an output as not reported, never the
  placeholder and never a guess (decision, 2026-09-24); the record prices for its prompt, and
  `usage_bucket.unrecorded_output` counts these records so every aggregate holding one reads
  as partial, a lower bound. The transcript head marks the reply the same way.
- *Version:* `rosetta_pricing::CATALOG_VERSION` combines the upstream commit, the data's
  SHA-256, the overlay's hash, the generator version and a digest of the rendered tables, so a
  generator change that alters any table re-prices projections whether or not the constant was
  bumped. `pricing_version()` adds the calculator's revision, bumped with any change to what a
  record costs under an unchanged catalog (3: dated tier and route rules, Bedrock and `@`
  spellings).
- *Coverage report:* `/api/pricing` lists every model in the open projection with unpriced
  usage — id, tokens, first and last hour, and whether the id is unknown to the catalog — so
  a model launched yesterday shows up the day it is first used, before anyone notices a low
  figure. Re-vendor genai-prices weekly and on any unknown id; the vendored copy went stale
  by one day when Opus 5.5 launched (3.49B tokens, $1,061, unpriced for a day).

**Re-pricing never replays the ledger.** `projection_state.pricing_version` records the
pricing version the rollup costs were computed with. A writer opening a complete projection
compares it with the compiled one. On mismatch, one transaction recomputes the cost
columns from the stored facts and stamps the new version; this took about 0.65 s for 327,520
usage rows on the 2026-09-11 corpus (diagnostic). A cold fold stamps the version it used.

**Pricing runs only for consumers that use it.** The general message decoder reads and
validates stored facts without computing USD. Transcript pages, exact-row lookups, team
timelines and report work explicitly add row costs; live mutations and usage rollups retain
their existing pricing. Search document builds/repairs, task/plan metadata, file targets and
Workflow attempt summaries skip the calculation because their outputs do not contain costs.

**Metadata readiness does not imply usage readiness.** The bounded metadata phase admits
session/project identities, titles and ownership, before complete transcript records are
streamed. Accurate cold totals need the streamed usage records, per-request model/tier facts,
and cross-file duplicate reconciliation. Moving that scan into metadata would delay metadata
readiness and duplicate stream work. A completed projection already serves stored totals on
warm startup; a fresh projection keeps aggregate readiness at final publication. Earlier
partial totals would need explicit coverage semantics and a writer-produced snapshot so
aggregate readers do not contend with cold ingestion.

**The headline is list-price equivalent.** The app leads with one basis across every agent:
what the recorded usage would cost at API list prices. Most usage runs on subscriptions, so
there is no bill to compare against. Provider-billed amounts appear as a secondary figure where
present. Partial or unavailable coverage is always visible — the `+` marker on any figure that
is a lower bound, `n/p` on a reply with usage but no priced part, the treemap's note for
projects it cannot size — and unpriced usage never reads as $0.

**Aggregates attribute a session tree to its root.** The heatmap, per-model days, hour
profile and lifetime contribution all scope the same way: every member of a tree whose root
belongs to the project, provided any member of the tree has been indexed. A subagent that
resolved another cwd still counts for the project its parent ran in, and a root whose own
transcript is gone (Claude Code's 30-day cleanup left 538 such roots) still carries its
indexed subagents' $277. The two paths can no longer disagree.

**Protocol v21, then v22, then v23.** `TokenSnapshot` gains the new fields, v22 adds
`requests`, and v23 adds `unrecorded_output` beside every spend figure and the coverage report
on `/api/pricing`.
Heatmap days, model days, hour totals
and contribution rows gain `cost_micro_usd`, `billed_micro_usd` and `unpriced_tokens` beside
`tokens`, so contribution rows can be ranked by spend. Timeline responses carry each
response's read-time cost. `GET /api/pricing` reports the catalog version, its sources and
coverage.

**Performance budget.** Diagnostic measurements on the 2026-09-11 projection, taken on a loaded
M1 Max: pricing costs 36–39 ns per usage row. The cold fold grows by 68–72 ms, from 545–582 ms,
and the busiest live bucket rebuild from 217 to 260 µs.

Cold profiling also distinguishes closed-plan preparation, reconciliation-tree cleanup,
complete bulk finalization, and publication's RAM release/reopen. Reconciliation preparation
and cleanup sit outside the canonical SQL timer, inside the enclosing apply timer. Consumed
source batches now release incrementally inside closed source application rather than in a
separate post-reconciliation destruction phase.
`serve --json --profile` emits discovery timing and a `serve-cold` profile at canonical
readiness, before live catch-up changes the counters. Detached search readiness is a later
boundary and must be measured separately; `--ticks 0` does not guarantee it completes.
The `live-catch-up` stage exposes the handoff; the search worker was woken at publication and
builds beside it. Its
`live poll time` contains filesystem walking and committed-evidence lookups, with separate
nested timers, a count of physical sources considered, and `live poll cursor reads` for the
sources whose cursor row had to be read and decoded; `live hint mapping time`, `live admission
time` and `live stream time` split the rest of a tick. Inventory-only layout streams
do not consume the live poll's stat work or source cap; admitted path lookup uses one
temporary index per poll. Watcher hints, cursor-relative recovery, snapshot membership,
normalizer expiry, and rotating audits retain their existing semantics.
The deferred partial `message_searchable(id, type)` index contains only the same user/agent
kinds selected by the search visitor. It covers the build's exact document count and supports
its id-ordered scan, avoiding a progress-only pass over the much larger message payload table.
It is built with the other cold secondary indexes and maintained normally in live mode;
query-plan tests bind it to the production search-kind predicate.

The frozen cold-ingest pair is campaign `usd-cost-abba3-2026-09-22`:

- *Setup:* `main` e8d907a against this branch, both release builds. The command was
  `rosettad ingest --agent claude-code --agent codex` at `nice 10`, run ABBA three times.
- *Corpus:* the read-only 750-file synthetic corpus, manifest sha256 `ed8d6a521b44763f…`.
- *Machine:* an Apple M1 Max (10 cores, 64 GiB). Other work kept the load average between 60
  and 400 throughout.
- *Logical counts* are equal: 315,168 messages and 2,464 buckets. The branch adds 472 usage
  samples, and its token total is 0.26% lower because thinking is counted once.
- *Medians:*
  - CPU time (user + sys) went from 35.4 s to 36.4 s (+3.0%).
  - Wall time went from 50.2 s to 49.8 s, within noise.
  - The set-based bucket fold went from 1.10 s to 1.35 s.
  - The projection file stayed at 672 MB.

The pair also caught a cost. Claude checkpoints were keeping each response's full usage snapshot,
which grew them by 50%. A live append rewrites the file's whole checkpoint, so that cost
recurs at runtime. They now keep a 64-bit fingerprint instead, and end 4% smaller than before.

The audit fixes (schema 24, 2026-09-24) were paired the same way, campaign
`audit-fix-abba3-2026-09-24`: `main` b85866d against this tree, the same corpus, command and
`nice`, ABBA three times, load average 33 to 455. Logical counts are identical on both sides
(315,168 messages, 2,464 buckets, 472 samples, the same token total: the corpus has no cross-file
copies and no placeholder responses). Medians: CPU time 38.0 s to 39.8 s (+4.8%); wall 60.2 s to
60.6 s, within noise; the bucket fold 1.37 s on both; canonical apply 15.5 s to 15.8 s; peak RSS
1.27 GB to 1.48 GB, the process-local usage-owner index plus the Claude adapter's per-response
state; the projection 672 MB to 688 MB, usage now on rows plus the `session_root` index;
checkpoints 44.0 MB to 48.8 MB. On the real 2026-09-23 ledger the checkpoint growth is 2.4%:
Claude Code +1.8%, Codex 1.47 MB to 2.33 MB for its per-file thread, parent and last-record
state, Grok unchanged.

**Prior art.** genai-prices (dated prices, the exactly-one-bucket rule), LiteLLM's Rust
`litellm-cost` (a compiled plan with explicit prompt-convention and threshold policies, and
Missing ≠ Null ≠ 0), Langfuse and Helicone (exclusive storage, integer money, cost stored at
ingestion), ccusage (the Codex request rule and replay skipping) and agentsview (dated rates and
integer microdollars priced from stored raw usage).

### 4.2 Normalization is a pure, versioned function

```rust
fn normalize(&self, raw: &RawRecord) -> Vec<CanonicalFact>   // no IO, no clock, no rand
```

Each adapter stamps a `normalizer_version`. Determinism + the ledger buys the core
doctrine:

> **The canonical store is a cache.** Bump the normalizer or the projection schema →
> rebuild derived state from the ledger. There are no migrations because there is
> nothing to migrate — the ledger is the truth, the DB is a materialization.
> (Spaghetti "drops the DB on mismatch" as a failure mode; rosetta does it as a
> designed, lossless operation.)

Differential replay is also the safety net for normalizer changes: run old and new
versions over the same ledger, diff the canonical output. That is what spaghetti kept
20k LOC of legacy TypeScript around to do — here it's a subcommand.

### 4.3 Analyzers (distilled data)

Derived/calculated data (token activity time series, per-session/per-day rollups, tool
usage stats, activity heatmaps) are **incremental fold functions over canonical facts**,
computed inside the same commit as ingestion (they're cheap) and stored in rollup tables
stamped with an `analyzer_version`. Same disposability rule applies: rollups rebuild
from canonical facts, canonical facts rebuild from the ledger. List-price cost is one such
fold: `usage_bucket` cost columns are stamped with the price catalog's version and are
recomputed from the stored usage facts when it changes (§4.1.3).

---

## 5. Layer 3 — Service provider

### 5.1 Transport decision: one daemon, localhost HTTP/WS

`rosettad` is a single daemon owning the store (single writer). Consumers connect over
`127.0.0.1` (axum: REST for queries, WebSocket for streams) — optionally a Unix domain
socket for the latency-obsessed. Why not NAPI like spaghetti: multiple consumer apps
(agent-view, chopsticks, future TUIs) shouldn't compile a native addon into every host
process, and localhost round-trips are microseconds — the speed argument never favored
in-process coupling enough to pay its build-matrix and lifecycle-coupling costs.

Escape hatch for bulk analytics: the SQLite file is WAL-mode and read-only-openable by
tools (sqlite-viewer style), explicitly documented as *unstable internal format*.

### 5.2 The three services

**Search (job 1):**
`GET /api/search?q=&agent=&runtime=&project=&session=&after_ts=&before_ts=&kind=&is_error=` → FTS5 match
with snippets and rank, with structured filters applied inside the ranked query. `q` uses the
backend-neutral product grammar: whitespace-separated required terms plus double-quoted required
phrases (4,096 bytes and 64 clauses maximum). Engine operators and field selectors are ordinary
text and never cross the wire. Any explicit time bound excludes rows whose timestamp was never
reported; unknown is not coerced to epoch zero. Unsupported kinds (anything but `user` and
`agent`, the indexed kinds), a set `is_error` (tool status is not indexed), blank selectors,
oversized selectors, and inverted ranges are HTTP 400 errors rather than successful empty
searches.
Each hit carries its row's canonical `kind` and reported timestamp beside the snippet
(protocol v17, 2026-09-09), read from the detached sidecar's own document table — so the
app's search sheet can say who said it and when without a second read per hit.

Protocol v18 (2026-09-09) adds the live-monitor shape of a search, driven by Godview's
"find the agent by what was said" sheet, which names the sessions it can jump to and
asks one question per keystroke: `sessions=` is a comma-separated set of at most 256
canonical ids matched as one `IN` scope (the server widens each id to every session that
rolls up to it, so a hit inside a delegated child answers for its root; an alternative to
`session`, never combined); `prefix=true` widens each term of three or more characters to
the tokens it begins while phrases stay exact (`puppet` finds `puppeteer`; the `*` is
appended outside the quoted token, so user text still never reaches FTS5 unquoted); and
`GET /api/sessions?native_session_id=a,b` resolves provider-native ids to canonical rows,
children included, without listing history. A sidecar generation that is merely behind the
canonical watermark now answers with its consistent snapshot labelled `catching-up`
instead of an empty page; only another projection epoch, an unpublished canonical, or a
failed/absent generation is `unavailable` with no hits. Blank selectors and oversized
sets remain HTTP 400.

**Observe (job 2)** — the flagship contract, successor to `observeSession`:

```
WS /ws/sessions/<id>?epoch=<epoch>&seq=<seq>
  server → { frame: "hello", protocol_version: 17, stamp }
  server → { frame: "timeline_snapshot", stamp, entries, timeline_meta, cursor, watermark }
  server → { frame: "mutation", stamp, mutation } …
  server → { frame: "timeline_meta_snapshot", stamp, timeline_meta }
  server → { frame: "resync" }                # gap/epoch change → replacement snapshot
```

On a resumable reconnect, the snapshot is replaced by unstamped replay mutations,
an optional unstamped metadata replacement, then `caught_up { stamp }`. Even an
empty replay ends with this checkpoint. `hello` advertises the replay target; the
client advances its resume stamp only after applying a snapshot, a stamped live
frame, or the complete replay. A disconnect halfway through replay repeats the
unacknowledged gap safely. Registration, replay capture, and its endpoint share
one hub lock. Async subscribers wait on `Notify`, with transport keepalive pings;
no thread or periodic polling bridge is allocated per socket.

The snapshot's cursor fetches the preceding page directly. HTTP timeline pages
and exact-row envelopes carry a generation watermark, read in the same SQLite
transaction as their rows. The TypeScript subscription owns visible rows, held
history, and anchors in one cache, applies live patches to all loaded identities,
and aborts outstanding reads on replacement. A stale watermark requests a fresh
snapshot. Notifications coalesce over 16 ms; ordered metadata caches update only
the changed collection and retain unaffected published arrays.

The contract lives in `rosetta-protocol`; `ts-rs` generates its TypeScript types.
Both HTTP's exposed `x-rosetta-protocol-version` header and WebSocket's `hello`
are checked before the client accepts data. Golden tests pin the JSON shapes
and check the Rust and TypeScript version constants. HTTP requests have a 30 s
default deadline and accept caller cancellation; changing the daemon origin
aborts requests and reconnects active subscriptions. Search cancels superseded
requests and ignores their late results.

`GET /api/runtimes`, `/api/projects`, root-only `/api/sessions`, and addressable
`/api/sessions/{id}` serve the metadata/browse side of the agent-view app; `/ws/meta`
streams the complete topology so descendant timelines can be embedded recursively.
`GET /api/sessions/{id}/timeline/{timeline_id}` resolves one exact canonical landing row;
scoped plan and file routes resolve bytes only after an explicit open action.

The expanded project activity calendar (2026-09-30) selects a recorded day, then
reads `GET /api/projects/{id}/usage/day?day=YYYY-MM-DD&offset_minutes=N` for its
24-hour model lines. This additive route uses the heatmap's session-tree scope,
timezone offset, and six token buckets; hourly models sum back to that day's
heatmap total. It reads existing `usage_bucket` rows without changing ingestion
or the projection schema. Cumulative session adjustments retain their recorded
hour rather than inventing a distribution through the day. Calendar hover only
previews a date; selection fetches the detail, cancelling superseded requests.
Streaks are marked on calendar dates; model shares and peak hours are drawn in
the selected-day chart instead of duplicating the compact summary's metric cards.
Hourly curves pass through their samples with bounded cubic tangents, so the
gradient fills and line glow do not introduce extra peaks or negative values.
For today's unrecorded hours, faint dashed guides continue the current level
without a fill or numeric value; the readout labels them as awaiting usage.

The project calendar and models chart open one shared detail dialog (2026-10-01),
with centered Activity / Models tabs and one set of fullscreen and close controls.
Each launcher selects its corresponding tab; switching keeps the same modal,
original opener, fullscreen state, selected day, model filters, and scroll positions.
Tabs support arrow keys, Home, and End. Each view mounts on first visit and stays
mounted while hidden, so opening Models does not request hourly activity and
returning to Activity does not refetch its selected day. Model usage still loading
when reached through Activity has an explicit loading state.

The Models tab enlarges the same 30-day streamgraph. Both sizes share the date domain, model
colors, wiggle baseline algorithm, and smooth paths. The summary groups models
after the top three under `other`; the detailed view retains every model as
its own band, legend entry, and hover row. The expanded presentation adds fading
gradient fills, subtle glowing outlines, and larger date labels and legend.
It stays a single plot, fits the viewport, and makes no hourly usage requests.
A vertical inspection guide snaps to recorded days, with markers at the band
centers from that same layout. Its tooltip shows the date, total, and each
model's tokens and daily share. Empty days show zero tokens and no calculated
shares or markers. Pointer hover and
keyboard arrows, Home, and End read the loaded values without interpolation;
the tooltip stays inside the plot and the period-total legend remains stable.
The legend uses compact columns; large model lists scroll within their bounded
legend or hover card while the plot and dialog remain in the viewport.

`WS /ws/collaboration/{id}` carries a complete collaboration-boundary snapshot,
content-free response model/effort observations, then filtered live changes. It
does not send private transcript windows or timeline metadata. Team roster
changes add/remove only the affected subscriptions. Opening a report's work
disclosure calls `GET /api/sessions/{id}/timeline/{timeline_id}/work`; the daemon
resolves the source session, response, and inclusive bounds from that canonical
report and returns only the proven range. Pending or missing evidence fails closed.

The transcript renderer keeps one covered history window (2026-09-28). Complete
team boundary subscriptions stay cached, but delivered events enter the document
only inside loaded parent coverage, never before its beginning or across an exact
anchor's unloaded gap. Undated shared events wait for complete history. Ordinary
worker fallbacks use that same coverage; exact invocation ownership takes priority.
Initial display starts at a primary chat boundary when one exists. Earlier reads
seek six primary messages with a five-page/1,000-row bound, hold the section until
reveal, and leave a clipped leading prefix in the subscription cache. Oversized
activity-only sections explicitly offer loading/retry inside their fold. Searching
for an older team delivery loads its surrounding parent history before landing.
Sitting and activity React identities survive overlapping prepends while canonical
anchors remain unchanged; image rails stay outside disclosures and retain their
scroll position. Full observed team histories, including currently undisplayed
rows, establish live-arrival baselines so paging cannot count old messages as new.
This is renderer composition over existing canonical pages, with no new projection
or protocol. Acceptance lives in the transcript history-window unit and Electron
fixtures alongside generation/resync tests for the data hooks.

Opening a session (2026-10-01) keeps its title visible and covers the reading
surface with a neutral conversation silhouette and an indeterminate meter. The
real document stays mounted, measured and inert underneath. Its first authoritative
snapshot and initial team boundary histories determine readiness; a failed team
read releases available chat with an inline retry, and a slow team read offers
“Read available messages.” Two render frames finish preview clamping, disclosure
layout and scroll placement before an opacity reveal. Initial layout changes snap
under the cover; ordinary live arrivals retain their existing smooth follow.
Search opens at its requested anchor, with an explicit escape to available chat.
There is no minimum loading time, invented progress, or wait for image downloads.

The timeline hook scopes rows, metadata, readiness and errors to the selected
session even in the render before subscription cleanup; late callbacks and earlier
pages from a closed subscription cannot publish into its successor. The column is
keyed by session, disposing its disclosures, pending viewers and scroll controller.
Only an authoritative empty snapshot can claim emptiness. Initial failures expose
retry through a fresh subscription (including a previously closed connection).
Ordinary reconnects and later roster additions keep a readable document visible;
a discarded authoritative snapshot re-enters opening. Loading is announced once,
its placeholders are decorative, and hidden controls cannot receive focus. Reduced
motion disables the meter and reveal animation. The design is recorded in
[transcript record §06B](../clients/app/design/transcript-record.html#session-opening);
`pnpm test:transcript-loading` runs delayed synthetic snapshots through production
hooks and Electron layout, covering the first visible position, team readiness,
retry, rapid switching, search, focus, themes and narrow columns.

Electron owns one managed daemon generation at a time. `serve --json` emits a
versioned readiness event with its bound loopback origin. `serve --announce` additionally
publishes `{schema: "rosetta-daemon/1", origin, pid, protocol_version, db, started_at}` as
`<pid>.json` under `~/Library/Application Support/rosetta/daemons` (macOS) or
`$XDG_STATE_HOME/rosetta/daemons` (`--announce-dir` or `ROSETTA_ANNOUNCE_DIR` override),
owner-readable, removed on orderly shutdown, and named in the ready event as `announce`.
It exists for sibling apps that did not spawn the daemon and therefore never saw its
stdout; a reader treats the file as a hint and probes `/api/status` before believing it,
because a killed daemon leaves its file behind. The app passes `--announce` (2026-09-09). Startup failure and
shutdown send SIGINT, escalating to SIGKILL after a grace period; replacement
waits for actual child exit and retries with capped backoff. Main sends revised
daemon state through the preload so the renderer reconnects when a new child
binds another port. Quit waits for daemon exit. Explicit external daemons remain
externally owned. Built renderers load from the registered `rosetta://app` origin,
which shares the daemon's HTTP and WebSocket origin allowlist with development;
opaque `null` origins remain disallowed.

**Metrics (job 3):**
`WS /v1/metrics?families=usage,lifecycle&agent=…` — live distilled feed. Hot path is
memory-first: capture → normalize → `tokio::broadcast` fan-out happens in parallel with
the DB commit, so monitor latency doesn't wait on fsync. Frames carry seq numbers and
the stream is at-least-once; anything display-critical reconciles via a durable REST
read. Slow consumers get coalesced updates + overflow signal rather than back-pressuring
ingestion.

#### Supervisor-enrolled runtimes (2026-09-12)

A sibling product runs one rosettad per user and manages agent *personas*: one vendor
data root per persona under the user's own directory, bound to the agent process through
the vendor's root variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, …). A user therefore has
SEVERAL runtimes of one kind — the OS-native `~/.claude` (discovered on the roll call,
consented by the user) plus N managed roots the supervisor owns. The projection side
always admitted this (`agent_runtime.id = runtime_id(kind, data_root)`); the serve side
now does too, through a start-time **enrollment file**:

- **`serve --enroll FILE`** (also `ingest`, `watch`, `rebuild`). FILE is JSON the
  supervisor writes, read once at start, tolerant of unknown top-level fields:
  `{"format": 1, "enabled_kinds": ["claude-code"], "runtimes": [{"kind": "claude-code",
  "data_root": "/abs/users/u1/agents/roots/claude-code/9a3d", "label": "Kim"}]}`.
  `enabled_kinds` is consent for DISCOVERED kinds given at start — the headless stand-in
  for the roll call, like `--agent`, but it never narrows the adapter registry the roll
  call shows; an unknown or uninstalled kind is warned about, never fatal. It is also the
  WHOLE of that consent, so dropping a kind from it revokes (see "authoritative" below). `runtimes`
  are explicit roots: `data_root` MUST be absolute (`~` forms are refused), `kind` a
  registered adapter; a duplicate, a root nested with another enrolled root, a root equal
  to or nested with the kind's native root, and a missing or non-directory root are each
  a startup refusal naming the entry (a missing root is a refusal, not a skip: the
  supervisor enrolls a root after creating it). `label` is presentation only.
- **Consent by construction.** Enrolled runtimes are never on the roll call
  (`/api/discoveries` lists native layout roots only), never part of the one-shot consent
  gate, always ingested and watched — an enrolled root that is still empty is watched
  from the first start, so the persona's first session appears live. With at least one
  enrolled runtime the daemon never waits in `awaiting_consent`; the roll call still
  publishes for the app. Every serve mode iterates one list of runtime **targets**
  (`{adapter, runtime, root, label, provenance}`; the census walk is the same for both
  kinds of target), and `consented` is a set of runtime IDS: a roll-call grant, a file's
  `enabled_kinds`, or (without a file) a persisted `enabled` flag for a kind maps to that
  kind's discovered target.
- **The file is the truth for enrolled runtimes on every start.** An enabled
  row with an absolute `data_root` is an enrolled runtime from an earlier start; if this
  start's file no longer names it, it is not watched and its rows stay public ("retired"
  for now). The predicate is the existing invariant that layouts never embed machine
  paths: a `~`-relative `data_root` is discovered, an absolute one is enrolled.
  (Errata 2026-09-12: this bullet also said "consent for discovered kinds still persists
  as `agent_runtime.enabled` on their `~`-rooted rows". That was true only without a
  file — with one it made the flag a second consent source, which could add but never
  revoke. Corrected by the next bullet.)
- **The file is AUTHORITATIVE for discovered consent too (2026-09-12).** With `--enroll`
  this start, consent for DISCOVERED kinds is exactly the file's `enabled_kinds` ∪ the
  command line's `--agent`/`--all-agents`. The projection's `enabled` flags are NOT a
  consent source — neither is the set carried over from an archived incomplete
  projection — because the supervisor's file is its record of what the user chose, and a
  union with the projection could only ever ADD: a kind un-checked in the app would be
  written out of `enabled_kinds`, and the restarted daemon would keep watching it from
  its own cache. Without a file, nothing changes: the persisted flags plus the one-shot
  roll-call gate remain rosetta's own app's consent, and `ingest`/`watch` already took
  the file and the command line only (`rebuild` is whole-projection from the ledger and
  takes no consent at all).
  - **The flags become a projection of the file.** At start (and again after a roll-call
    grant) every persisted runtime row is re-derived from this start's consent: not
    consented → `enabled = false`, consented → `enabled = true`, so `/api/discoveries`
    and `/api/runtimes` never advertise a consent nobody is acting on, nor hide one that
    is. Re-enabling is not symmetry for its own sake: the identity upsert deliberately
    never resets `enabled` (re-discovery must not revoke a user's choice), so a row an
    earlier start retired would otherwise read disabled forever while being watched.
    Retiring touches the flag only — sessions, timelines and ledgers stay public, and an
    explicit `rosettad rebuild` still re-enables every runtime it replays.
  - **A pending cold generation gives way to the file, and never blocks the start.** The
    generation is durable evidence, not standing consent. A runtime it holds that this
    start's file no longer consents — a discovered kind un-checked mid-backfill, or a
    persona the supervisor retired — is DROPPED from the resume, named on stdout, and the
    daemon starts and finishes the generation for everything still consented. Its staged
    evidence is never touched: it publishes with the ledger and `rosettad rebuild`
    recovers it, so the drop costs rows, not bytes. This replaces the fail-closed refusal
    for a retired enrolled runtime ("add it back, or run `rosettad rebuild`"), which
    stranded every other runtime behind a manual step whenever the supervisor did an
    ordinary thing. With nothing consented at all, the cold gate still applies: the roll
    call publishes and the daemon waits in `awaiting_consent` rather than exiting or
    publishing a projection it has no consent to build.
- **Ledger directory per runtime, backward compatible.** A discovered runtime of kind K
  keeps `<projection>.ledger/K/`; an enrolled one uses `<projection>.ledger/K~<digest>/`,
  `<digest>` = the first 12 hex characters of blake3 over the absolute `data_root`
  (`~` never occurs in a kind id). Opening a runtime's ledger writes
  `<dir>/runtime.json` = `{"format": 1, "id", "kind", "data_root"}` when absent
  (identifiers only; discovered runtimes are stamped additively too), and `rebuild`
  reconstructs each runtime's identity and root from the stamp. A directory without a
  stamp is the legacy per-kind convention (kind = directory name, root = the layout's
  `~` form). The cold-generation manifest is format 2: it adds
  `runtimes: [{id, kind, data_root, ledger_dir}]` and still reads format 1.
- **Labels and provenance are serve-time facts, never persisted.** `RuntimeRow` carries
  `label` and `provenance` (`discovered | enrolled`) on the wire, both omitted when
  absent/default so an undecorated row — and every ledger meta frame — serializes as
  before; `/api/runtimes`, the `/ws/meta` snapshot and its runtime mutations fill them
  from the in-memory enrollment for enrolled rows. `DaemonStatus.runtimes` and the
  backfill meters are keyed by `runtime_id` and carry `label` beside `kind`. All additive
  (serde defaults): `PROTOCOL_VERSION` stays 20.
- Deferred to §11: live mutation of the enrolled set, and rediscovery without restart —
  the supervisor restarts the daemon when the set changes.

---

## 6. Storage

- **SQLite** (rusqlite, bundled), WAL, `synchronous=NORMAL`, single-writer daemon,
  batched transactions. One live ingest transaction applies resolved canonical rows, rollup
  corrections, minimal cursor/provenance watermarks, and coalesced search repair keys.
  Normalized operations, pending joins, opener copies, evidence winners, and replacement payloads
  stay in RAM. Canonical rows, rollups, joins, and the generation watermark remain atomic; after a
  crash the ledger recreates the discarded accumulator instead of SQLite acting as an event log.
  Ranked session titles follow the same boundary: SQLite retains one selected title and bounded
  winning provenance, never the losing candidate set.
  Session cwd/project ownership is admitted once from bounded adapter metadata. Adapters
  may provide an agent-neutral checkout-group root or a same-project native-session edge;
  the meta resolver closes those edges before journaling. Otherwise it falls back to the
  local Git common directory, which merges linked worktrees but deliberately keeps ordinary
  clones of the same remote separate. Stream normalization may retain a current execution
  cwd in checkpointed adapter state solely to
  resolve relative tool arguments; it cannot write that state to canonical tables, consult today's
  checkout to enrich history, create a project, or reassign a session. Filesystem-derived project
  identity is captured during meta ingest and written into the ledger before it becomes canonical.
  The store rejects `SessionMeta` in every source-scoped stream batch, making this a write-boundary
  invariant rather than an adapter convention.
- **Content-addressed image members** are part of the one canonical projection, not a third
  representation. An adapter may recognize and decode a verified native inline image, but only
  the store writes it to `<projection>.assets/<blake3>.<ext>`. Live ingestion makes the immutable
  file durable before committing its `/api/assets/<blake3>.<ext>` URL. Private builds sync all new
  members before database publication; monotonic content-addressed additions let the final
  SQLite file remain the only atomic swap. The raw ledger keeps the original inline form.
- **A generation this daemon cannot use is no generation.** A well-formed `CURRENT` manifest
  whose search schema or format differs reads as absent (2026-09-09): the worker rebuilds, status
  reports `building` with the rebuild's progress, and a query answers unavailable — a schema
  bump is a routine rebuild, never a `failed` install. After a generation publishes, every other
  regular file in `generations/` (superseded generations, their WAL/SHM, the temp file of a
  build that died) is pruned; the sidecar is disposable and a reader still holding a superseded
  file keeps its inode until it closes.
- **Detached SQLite FTS5 sidecar today**, behind the backend-neutral search/readiness
  seam. A worker builds private immutable generations, atomically selects one with
  `CURRENT`, repairs complete sessions by epoch/revision, and acknowledges only after the
  search commit. Search loss or corruption never blocks canonical reads and triggers a
  rebuild. The disposable Tantivy prototype clears latency/build/filter/determinism gates
  but not relevance, product-mix, or storage acceptance, so it is not production.
- **On-disk layout:** `<projection>.ledger/<agent>/` (`RLZ1`/`RLG1` segments plus the derived
  `.segments` index, and since 2026-09-12 a `runtime.json` identity stamp; a
  supervisor-enrolled runtime of the same kind uses `<projection>.ledger/<agent>~<digest>/`,
  §5.2) + the configured SQLite file +
  optional canonical `<projection>.assets/` members; `--ledger` can relocate the segment root.
  An adopted search backend owns a disposable `<projection>.search/`. Ledger is precious; the
  canonical projection and search index are rebuildable.
- Keep the schema compact rather than chasing a numeric table target. Entities, rollups,
  drift, cursors, generation staging, and the proposed search revision/repair set each
  need a named consumer; a new table without one is not justified.

## 7. Performance playbook

> The detailed implementation design — the pure op-stream pipeline, crate seams,
> stream-phase concurrency, and the bench/profiler infrastructure (bench-corpus
> generator, per-stage criterion benches, `rosettad ingest --profile`, the rebuild
> headline bench) — lives in [`docs/ingest-engine.html`](ingest-engine.html)
> (rendered design doc, 2026-08-27). Where it and this section disagree, the HTML
> is newer.

### 7.1 Cross-adapter applicability checkpoint (2026-08-27)

The store hot path is shared, but the evidence used to optimize it must not be. The
committed canonical-op goldens and restart tests currently establish this matrix:

| adapter | driven source | tool-result merge | usage representation in fixtures | restart proof |
|---|---|---:|---|---|
| Antigravity | 1 JSONL + metadata SQLite snapshot + per-conversation SQLite snapshot | yes | `session_usage` samples from the conversation database | split/resume + ranked-title ledger/snapshot replay |
| Claude Code | 2 JSONL streams | yes | separate identity-stamped `Usage` merge behind each response row | split at every fixture line |
| Codex | 1 JSONL + metadata SQLite snapshot | yes | separate `Usage` merge | split/resume + native-title snapshot replay |
| Copilot | 1 JSONL + metadata document snapshot + session-store SQLite snapshot | yes | `session_usage` samples (per-model shutdown intervals) | split/resume + metadata rename replay |
| Grok | 1 JSONL + metadata document snapshot | yes | separate `Usage` merge | checkpoint/one-shot + document replay |
| Hermes | 2 SQLite snapshots (state + profile state) | yes | `session_usage` samples (per-model session rows) | stateless snapshot replay equality |
| Kimi Code | 1 JSONL + metadata document snapshot | yes | inline on messages | split/resume + document replay |
| OpenCode | 1 SQLite snapshot | yes | separate `Usage` merge (one per step-finish part) | split at every snapshot record |
| Pi | 1 JSONL | yes | inline on messages | split/resume state test |

“None observed” is fixture evidence, not a claim that a future release cannot report
usage. [`adapter_optimization_matrix.rs`](../crates/rosettad/tests/adapter_optimization_matrix.rs)
fails if the roster, stream-driver contract, checkpoint round trip, or canonical op
classification changes. Every adapter has full canonical-op goldens; Hermes' snapshot
projection is frozen alongside the eight existing golden corpora.

The newer frozen-source campaign adds private real-source evidence for Antigravity,
Claude Code, Codex, Grok, Hermes, and Kimi Code. Copilot, OpenCode, and Pi were not
installed and remain fixture/restart evidence rather than real-source performance
evidence. Hermes now supplies a non-empty SQLite snapshot lane, although its 25 messages
are too small to extrapolate snapshot-driver throughput. The six non-empty runtimes
produced 773,375 messages with exact FTS, usage-bucket, tool-call, and token aggregates,
zero generation/topology/ordinal/staging defects, and ranked-search-digest-equivalent
cold and ledger-rebuilt projections. Semantic backlog remains explicit: 550
Claude metadata-only sessions are Pending and 79 result records are unmatched. Seven of
those results have a call only in their parent/root session, so cross-session correlation
is not yet closed. The stream also retained 195,484 unknown-type/no-op outcomes plus
parsed-record unknown-field drift; the newer snapshot retained 195,959 unknown outcomes.
Zero hard errors means preservation succeeded, not that every current native shape has
canonical semantics. This paragraph is the retained pre-closure performance baseline;
the 2026-08-28 semantic audit subsequently closed document metadata admission, current
writer/type vocabularies, session-scoped usage, project identity, and client visibility.
Evidence-blocked and intentionally non-canonical rails are listed in
[`docs/ADAPTER_AUDIT.md`](ADAPTER_AUDIT.md), rather than being implied by a raw unknown count.

This makes the optimization boundary explicit:

- Presentation renumbering is below the adapter boundary. A retained frozen replay
  showed 5,918 per-session window/sort passes consuming 220.410 s. Private first
  generations now keep collision-free provisional ordinals and canonicalize every
  session once at finalization; live and replacement swaps retain transactional
  per-session ranking. On the same ledger this reduced wall from 513.58 s to
  317.49 s (−38.2%) and comparable canonical-plus-ordinal work from 302.092 s to
  88.671 s (−70.6%). Exact logical and ranked-result equivalence passed. Peak RSS
  rose from 190 MB to 502 MB, so partitioning that finalizer is a future measured
  memory/speed tradeoff, not a free follow-up.
- Batch session timestamp touches and RAM-only late-arrival reconciliation are roster-wide:
  all captured adapters exercise the shared message/tool/usage store paths. A batch orders
  openers before updates, folds each tool correlation once, and writes the resolved row once.
  Cross-file orphans wait in process-local maps indexed by relation target and source
  cursor/generation. Openers look up only matching pending fragments; activation and
  abandonment look up only the affected source generations. Ordinary append/stage batches
  never scan unrelated pending state. All index updates happen after the SQLite commit,
  including changes in ownership when a newer usage snapshot replaces an orphan. A restart
  intentionally discards these maps and replays the affected ledger scope.
- Same-batch tool-result folding is roster-wide in semantics, but the measured SQLite
  implementation is rejected. On the frozen mixed ledger it safely folded 358,790 of
  360,052 tool results plus 149,755 of 151,298 separate usage snapshots (99.45% overall)
  and retained exact logical counts. Fully populated message inserts nevertheless raised
  message-write time from 9.327 s to 17.113 s and canonical apply from 36.923 s to
  42.805 s (+15.9%). The candidate was reverted at `0d5796d`; schema 11 instead performs a
  single RAM fold before canonical application without a persisted pending path.
  Inline usage remains part of message application, while separate usage ops currently
  come from Codex, Copilot, and Grok.
- Larger live JSONL tail batches may apply to seven adapters, but the analogous private
  ledger-replay experiment is rejected. Raising the replay boundary from 512 to a
  source-byte-bounded 2,048 ops cut batches from 14,301 to 12,275 and commits to
  3.754 s, while canonical apply regressed from 36.923 s to 48.171 s (+30.5%) and
  peak RSS rose from 511 MB to 673 MB. It was reverted at `93a2ece`. Hermes and
  OpenCode still require a separate SQLite snapshot-query batch/memory experiment;
  no JSONL or ledger-replay result may be projected onto them.
- A generation seal already durably checkpoints activation/abandon, but removing the
  second clean final checkpoint is not a retained performance optimization. Exact
  post-seal dirty tracking skipped 5,918 of 14,301 replay batches while preserving a
  byte-identical `ingest_cursor` table and exact projection totals; commit time still
  measured 9.715 s inside its retained 8.5–11.4 s range. The candidate was reverted at
  `52a533e`. Replay batch count is not a performance proxy on this storage configuration.
- Payload-light ledger election is below the adapter boundary and therefore roster-wide,
  but the RLG1 implementation was rejected. Paired frozen scans saved only 0.19–0.34 s
  (0.9–1.6%) of wall time even though one pair reduced user CPU from 8.66 s to 0.28 s;
  seek-heavy variants regressed. Do not carry the extra two-pass safety surface. Any future
  shortcut needs a writer-produced authenticated control index or footer that the full
  checksummed replay verifies.
- Raw ledger framing and buffering are also below the adapter boundary, and the latest
  frozen-source pair separated them. Borrowing payloads removed the temporary raw-frame
  allocation but moved total ledger time from 18.237 s to 18.394 s, so it was reverted.
  Raising only the writer buffer to 1 MiB reduced buffered write plus explicit flush from
  7.950 s to 6.522 s and total ledger time to 16.988 s; `25d18e9` retains that change.
  Source checksums, projection aggregates/integrity, segment shape, all post-meta ledger
  bytes, and 100-cycle ranked-search results matched. The measured cost is about 5 MiB peak
  RSS across six open runtime writers; the 7.41 s whole-wall delta is not attributed because
  FTS alone varied by 6.406 s.
- WebP projection and display renditions (`webp-image-pipeline-20261001-cpu`)
  use one permission-read-only, SHA-256-checked synthetic source corpus: 20 Codex
  sessions, 20 images (photo, UI, dark/light text, alpha, four variants each), and
  the same eight-query suite. On Apple M1 Max / macOS 26.5.2, release builds, three
  fresh ingests per side, baseline `18270fd` versus the final implementation,
  median process CPU moved **0.205 → 2.134 s**: the intentional one-time image
  preparation cost is about 96 ms extra per image in this sample. Median elapsed
  ingest was **0.531 → 2.898 s** in that pair; an earlier final-build pair under
  heavier system load took **0.986 → 14.114 s**, so elapsed time alone must not
  be attributed to codec work. All runs are retained, including the earlier
  **0.334 → 2.207 s** pair before the final cache-retention review.
  Cache disk usage including every rendition moved **15,663,606 → 2,825,420 bytes
  (-82.0%)**; a row of 20 tiles requests **285,070 bytes instead of 15,663,606
  (-98.2%)**. Five alternating Electron 43.4.1 / Chromium 150 fetch+decode runs
  (four concurrent loads, browser cache disabled) moved median row resource loading
  **248.4 → 45.1 ms (5.5×)** and decoded pixels **27,097,600 → 2,048,000 (-92.4%)**.
  The earlier identical-image Electron sample measured **238.3 → 29.1 ms (8.2×)**.
  These isolate resource loading, not end-to-end scroll FPS; the PNG-heavy sample
  is not a universal compression ratio. Every ingest retained counts 1 runtime /
  1 project / 20 sessions / 40 messages and identical ordered search results
  (SHA-256 `3413f05ecba4b47ed779190af023791f11695bd360fcd5256ad28ff2320712de`).
  A schema-25 fixture ledger rebuilt to 26 with unchanged message IDs/text and
  byte-identical read-only ledger files; obsolete originals were reclaimed.
  Cleanup retains workflow-owned assets and images inside externalized documents,
  and skips the JSON scan altogether for archives without image cache members.
  [Retained evidence and reproduction commands](../tools/image-bench/results-2026-10-01.json)
  include source/query/binary checksums, every run, machine/configuration, and
  the local raw artifact directory. The scripts live beside that evidence.
- Inline-image recognition is adapter-specific; content-addressing, durability, and serving
  are shared projection facilities. The retained `image-assets-2026-08-29` pair used the same
  checksummed, permission-read-only 88,498,056-byte Claude transcript on one macOS/aarch64
  machine. Its 56 image records decoded to 54 unique original PNG/JPEG files (15,489,826 bytes).
  SQLite fell from 43,712,512 to 22,622,208 bytes (-48.2%); SQLite plus image members was
  38,112,034 bytes (-12.8%), because decoded binaries avoid Base64 expansion. `tool_payload`
  fell from 29,220,858 to 8,184,806 bytes (-72.0%). Canonical application moved from 1.074 s
  to 1.017 s (-5.3%) even after 40.3 ms of image materialization; tool-result writes moved
  from 45.7 ms to 19.0 ms. The raw ledger remained 89,294,939 bytes and therefore lossless.
  Every decoded byte and every non-image payload field matched. A frozen 100-cycle product-query
  campaign retained exact hit counts and ranked-result digests; p95 moved about +1.1% in both
  search lanes, which is treated as run noise because detached FTS does not read `tool_payload`.
  This is one payload-heavy file, not evidence for whole-roster wall-time extrapolation.
- Private-cold relational reconciliation is now a generation-scoped RAM fold rather than a
  per-fragment SQLite loop. The `cold-tool-ram-reconcile-2026-08-30` pair used that same
  permission-read-only 88,498,056-byte Claude transcript (SHA-256
  `7ecfbffce129ee8c0fc07e2b0b28325f569227cc32cb1894aa9c67327dc02c96`) on the same
  macOS/aarch64 machine, release builds, and identical CLI/configuration. Baseline `d4abca8`
  versus the final, retry-hardened candidate moved canonical apply from 4.079 s to 0.171 s
  (-95.8%), total store apply from 4.172 s to 0.349 s, and process wall from 5.02 s to
  1.29 s. The candidate spent 26.3 ms folding 2,566 correlations in RAM and 32.1 ms
  inserting 5,132 reduced facts;
  tool-result lookup/write timers fell from 1.799 s/59.6 ms to zero because each final tool
  row was inserted once. One max timestamp touch per affected session reduced touch attempts
  from 8,011 to 2,880. Deferring tool rows moved 3,926 provisional ordinals into the global
  38.1 ms renumber pass, which is included in the wall result. Both sides retained 24,854
  parsed records, 12,557 ops, 3,934 messages, 2,566 tool rows, 5,132 reduced inputs, 868 edits,
  equal usage/aggregate rows, zero logical `EXCEPT` differences after mapping
  surrogate message IDs, and passing quick/foreign-key checks. Frozen search campaign
  `67b44f4c6ec55edea241aaeb6760b725580d8d1c52fae599b2e00b6fbc9a6b2a` used the
  checksummed `product-diagnostic-v2` suite (BLAKE3
  `00dcab42b607869ed696fe70caed9ab16c7ffe3507453e8f3ffc2ebccbc4fbe1`), 5 warmups and
  100 cycles; the comparison passed equal schema/configuration, logical counts, filters, hit
  counts, and ordered-result digests. This optimization applies to every adapter emitting the
  universal `Message::ToolUse` / `ToolUpdate` / response-usage / file-edit vocabulary. The later
  schema-11 storage-boundary change removes the reduced-fact writes too; its performance claim
  requires a new frozen before/after pair rather than reusing this earlier measurement.
- Enforcing CHECK constraints while private cold rows are written, so that the final
  `quick_check` could skip CHECK evaluation, is store-wide and rejected. SQLite re-checks a
  CHECK on every write that touches its columns, so the gate would hold, and on the
  2026-08-26 frozen snapshot the final `quick_check` fell from 4.4–4.8 to 1.0–1.9 CPU-s. But
  cold rows are written through single-row prepared INSERTs, and every execution of the
  message INSERT rebuilds nine ephemeral b-trees for the CHECKs' `IN (...)` lists of three or
  more constants (53 keys): about 7.0 CPU-s for 357k tool rows and 1.1 CPU-s for 153k other
  rows. Claude's tool-row writes sit in its admission on the critical path; at load ~190 they
  went from 7.6 s to 36.0 s while validation fell from 16.9 s to 5.9 s. The candidate is the
  last commit of `cold/t1-final` (2026-09-27), not merged. It needs batched cold inserts or
  CHECKs without long `IN` lists first.

The applicability is general; unpaired saving estimates are not. Always-on split counters
attribute replay cost by adapter and canonical statement family. The former target-aware SQLite
pending gate at `8fc0b50` was useful historical evidence (7.905 s to 0.424 s on the frozen
ledger), but schema 11 deletes that SQL path entirely. A new frozen campaign must measure the
RAM-only implementation before any further wall-time claim is merge-admissible.

The schema-11 storage-boundary validation replayed the same permission-read-only 26-segment
Claude ledger twice after the final code change: 1,241,440 records, 5,445 sources, 5,933 sessions,
329,955 messages, 208,124 resolved tool rows, and 41,786 file edits. Final release wall was
95.43 s (the preceding run was 95.90 s), with 24.818 s canonical apply, 9.346 s normalization,
6.645 s global renumbering, 17.959 s detached search build, passing `quick_check`/foreign keys,
and zero private reconciliation objects. The canonical SQLite file fell from the retained
schema-10 artifact's 1,344,462,848 bytes to 666,062,848 bytes (-50.5%); detached search stayed
effectively flat at about 394 MiB. `tool_update_fact` plus its two indexes had occupied about
447 MiB, while removing opener/merge state from `message` cut that table from about 503 MiB to
274 MiB. Public message/tool/title/file-edit/usage rows matched after excluding surrogate ids and
source-provenance keys. The audit also found 6,415 historical `workspace_obs.git_repo` values that
the old live path had inferred from a checkout but never captured in the ledger. Schema 12 removes
that relation entirely: one primary cwd and its meta-settled local checkout identity are captured
in the ledgered meta phase, and stream projection has no ownership mutation op. Peak RSS was 3.64 GB, so partitioning the disposable cold RAM
fold is the next measured memory target. There is no speedup claim here: an exact same-machine
schema-10 rerun is still required before attributing the roughly 95-second absolute wall.

The private-generation insert-first candidate at `207bc37` exercised all 786,004 message
attempts and fell back on 17,724 legitimate repeated timeline identities. Message identity
lookup plus write moved from 22.601 s to 20.859 s (−1.742 s / −7.7%), but total canonical
apply regressed from 36.923 s to 38.065 s. SQLite still probes the unique timeline index
inside an insert, so the explicit 13.275 s lookup timer was not an independently removable
cost. The five campaign cardinalities matched exactly; the performance gate failed before
ranked-search comparison, and `c6319e6` reverted the candidate. The current path therefore
retains measured ceilings of 13.275 s for message identity lookups, 9.327 s for message
writes, 4.286 s for tool-result writes, and 1.034 s for session touches. These are costs,
not additive saving
promises. The corpus now has one non-empty SQLite snapshot source, but it is not large
enough to transfer JSONL batching or throughput results to Hermes/OpenCode; those claims
still require a driver-specific frozen pair or a declared production-weighted agent mix.

- sonic-rs (or simd-json) for JSONL; rayon parallelism across files for backfill;
  blake3 for content hashes and ledger-frame checksums.
- Batched writes; prepared-statement caches; mmap reads for big transcripts.
- Private cold loads capture adapters into independent RAM planners, precompute presentation
  order, and feed closed runtimes to one canonical writer. A normal fresh `serve` streams instead:
  each source enters the canonical writer as a closed source while capture continues, and each
  runtime is admitted (fold, final tool rows, Pending flip) when its capture closes. Fresh serve
  places that canonical schema in named shared memory: metadata is immediately public and each
  completed runtime becomes transactionally readable before the global set-based gate. It then
  installs one validated disk copy and atomically retargets readers. Cross-session aggregates are
  unavailable until that retarget, and the app bounds their post-publication fan-out. Existing
  complete projections enter live maintenance immediately and run only bounded metadata plus forced
  cursor-relative catch-up; legacy incomplete disk projections keep the per-file WAL resume path.
  FTS is always a detached generation. Live batches
  and replacement swaps remain transactionally canonical and rebuild each distinct affected usage
  bucket once.
- Detached search construction resolves message/tool column positions once per SELECT and reuses
  the ordinary typed decoder and its validation. Single-row callers retain named-column decoding;
  cached positions are derived from the actual statement, including session-scoped repair scans,
  and so are the document's id, runtime, agent, project, timestamp and kind columns (by-name
  lookups had cost more than decoding the entries). The canonical snapshot keeps ordinary reads,
  not a memory map: the map saved little and would turn a read I/O error into a crash. The
  unpublished FTS generation uses a 512 MiB pager
  cache, released when its build connection closes, so a whole generation (about 250 MB) never
  spills and is never read back from the file during its index, check and commit. The generation
  carries one metadata index, `search_document_session`, for live repair's per-session deletes:
  with no planner statistics every filter combination the backend accepts plans from the FTS
  match and joins documents by rowid (a unit test checks all 384 shapes), so the runtime, agent,
  project, ts, kind and error indexes and the database-wide `ANALYZE` were pure build cost.
  Generations built before 2026-09-27 keep those indexes and statistics and remain valid under
  search schema 4; with statistics, only five-or-more-filter combinations mixing API-only runtime,
  session or time filters with a very rare value ever planned through them. Publication is gated
  by one `PRAGMA quick_check` on the read-write build connection: in the bundled SQLite (3.44 and
  later) it also runs FTS5's integrity method, the same checksum verification as the
  `'integrity-check'` command, which the build used to issue as well, re-tokenizing every
  document twice; a unit test pins that `quick_check` reports FTS5 index damage. Search
  extraction, member verification, the page walk, the FTS5 verification, syncs, and atomic
  generation publication remain part of the measured work. SQLite documents this
  [connection-local, demand-allocated cache setting](https://www.sqlite.org/pragma.html#pragma_cache_size).
  Frozen campaign `rosetta-cold-ingest-20260906` compared the retained `7e6b873` working-tree
  binary with this change on an Apple M1 Max / 64 GiB, macOS 26.5.2 arm64, Rust 1.97.1 release,
  system volume, warm filesystem cache. It reused 200 read-only Claude Code/Codex JSONL files,
  200,000 records / 316,975,655 bytes; source manifest SHA-256
  `f7c85b319eeb1779a0e29f3dba39bc6be29a221cda09a082109c37b3a3b2fb93`.
  Three alternating pairs used `ingest --experiment-parallel-cold
  --experiment-cold-completion-order --experiment-async-ledger --experiment-sqlite-in-memory`
  with identical enrollment, source snapshot, and profiling. Full-command median elapsed fell
  **13.718 → 11.677 s (−14.9%)**, with ranges 13.510–15.258 and 11.021–12.548 s;
  detached-search median **8.175 → 5.710 s (−30.2%)**; median peak RSS
  **1,448.9 → 1,421.4 MiB**. An additional ordinary fresh-`serve` pair, polled through
  `/api/status` every 50 ms without a renderer, reached search-ready at 12.852 → 10.577 s;
  canonical-ready was 5.801 → 5.292 s. This is corroborating evidence from one readiness pair.
  All six finite builds, both fresh
  serves, and a candidate replay of the frozen baseline ledger have identical normalized digests
  across 17 canonical tables and all five search lanes: 120,151 messages/search documents,
  200 sessions, 5,485 file edits, 836 usage buckets, zero drift. Normalization excludes only
  process-time/audit/checkpoint bookkeeping and maps surrogate IDs to stable identities.
  The required `search-bench` comparison passes with 16 product-grammar diagnostic cases
  (six return hits), five warmups and 100 measured cycles in each shared/fresh backend lane:
  6,400 measured queries across the pair, identical counts and ranked-result digests. Its frozen
  campaign is `bedd27c9d6e301126edcdcce72b1d09ece99328318bcf980ef85ce2955fa3f27`;
  query-suite SHA-256 is `9ce28aa99fe0eeda42a3769872b46deb993200b1e696424e3b40f4ee076ace44`.
  This diagnostic mix does not establish production query weighting. Workspace validation passes
  842 tests (one opt-in watcher test ignored); formatting passes, and workspace/all-target Clippy
  passes with only the existing `large_enum_variant` category excluded.
  Initial exploratory runs that exhausted disk headroom were discarded and both sides rerun;
  retained repeat databases are losslessly archived with verified per-file hashes.
- A first cold projection now requires both the projection and ledger destination to be
  absent. It buffer-flushes into a same-filesystem named `<ledger>.staged` tree whose durable
  manifest preserves enrollment/root binding across process exits, syncs every staged segment
  plus child-to-parent directory entries, atomically promotes the whole tree,
  syncs its parent, and only then installs the validated SQLite file. A missing projection
  beside an unmarked existing ledger normally fails closed to `rebuild`, preventing generation
  collisions. A marked staged/public cold generation instead replays sealed evidence into RAM and
  finishes raw-only sources at a strictly newer generation. `--restart-incomplete` archives only
  explicit legacy artifact sets under the projection lock.
  Existing/live projections retain ledger-sync-before-visible-SQLite ordering. The frozen
  pair measured segment syncs at 7,435 → 66 and ledger time at 96.64 s → 18.02 s; twelve
  directory barriers cost 0.0005 s. The zstd container's pair (2026-09-04; synthetic
  claude-code corpus of 400 files / 113,006 records / 1.114 GB with a sha256 manifest, seed
  20260904; release builds of de8f074 vs ac4f142 in separate worktrees; Apple M1 Max carrying
  unrelated load 67→15; three alternating rounds, page cache warmed): ledger on disk
  1,118,207,122 → 199,120,503 bytes (5.62×); ingest wall 42.2/39.6/43.7 s → 47.9/40.1/42.8 s
  (rounds 2–3 within ±2%), ledger worker time 2.4–4.0 s → 4.5–5.3 s, user CPU +3.5 s,
  peak RSS 2,410 → 2,408 MiB; rebuild 34.9/35.8/37.8 s → 54.5/37.4/38.9 s with ledger read
  2.4 s → 3.7–5.1 s per replay (the decode; the raw side read from cache); 113,807 frames,
  146,636 ops, and every projection table equal on all six runs. So: storage 5.6× smaller,
  ingest unchanged within noise because the work lands on the ledger worker, and a
  warm-cache rebuild a few percent slower — the cold-cache case, where 5.6× fewer bytes
  come off disk, was not measured. Search is independently disposable and may never
  delay canonical correctness validation.
- Frozen campaign `cold-topsteps-20260925` covers the owned cold-batch transfer,
  borrowed opener selection, driver-only indexed live polling, and deferred partial search
  index described above. It reuses the permission-read-only Claude/Codex snapshot from
  `cold-sources-20260826`: 57,175 files / 10,993,320,551 bytes, manifest SHA-256
  `7428042f111a62a3b0214f92e0345eb8b2e786661201a3ae3bf5c5d4e7b69e1c`; query-suite SHA-256
  `12e38e5ecc9a0e3d89bdd15b4c9fcc429defd07325f685cb108e91a97465aee7`.
  Serial single-agent release `serve` pairs on the M1 Max/64 GiB Mac, with SamsungRed outputs
  and un-evicted filesystem caches, match all 17 normalized canonical tables, every asset,
  and 8,000 measured ranked-search queries. Source hashes and file sets were reverified.
  Claude's publication-to-observed-indexing interval fell 13.70 → 1.57 s. On the same
  candidate database, forcing a payload-table count scan took 5.419 s versus 0.00937 s
  through the covering partial index (Codex: 2.417 versus 0.00337 s); the full visitor also
  uses that index in id order. Whole-startup percentages are deliberately unclaimed:
  Claude reached full readiness at 129.46 → 124.85 s, Codex at 41.85 → 57.87 s, and an
  unchanged-binary Codex control later took 182.48 s on identical input as host load reached
  424. RSS is likewise confounded by compression/paging. The ordinary regression suite
  passes 256 tests (one opt-in test ignored), and affected-crate/all-target Clippy passes
  with warnings denied. Exact binary/code hashes, commands, stage/resource observations,
  equality results and harnesses remain in `/private/tmp/rosetta-cold-topsteps-20260925/`;
  derived data trees were removed only after the pair and query checks passed.
- Frozen campaign `capture-opt-20260925` reuses that same read-only snapshot and query suite
  for bounded raw-ledger grouping and private async checkpoint coalescing. Small records share
  the existing batch-frame format with a 1 MiB body cap; large records remain direct. Private
  intermediate compression flushes coalesce until metadata, a source seal or rotation, while
  the final joined sync barrier and ordinary live durability remain intact. Claude/Codex frame
  counts fall 1,221,418 → 24,130 / 885,677 → 6,754; flushes fall 11,572 → 8,206 / 1,890 → 783.
  Alternating synchronous ledger-only probes on frozen large files measured mean time reductions
  of 12.5% / 21.5%, with identical decoded records. A delimiter-scanning experiment was reverted
  because warmed-cache results did not establish a robust improvement. Full fresh-serve readiness
  was 93.01 → 1,583.37 s / 54.19 → 286.47 s under sharply different host pressure, so no full-startup
  speedup is established: load reached 737 on ten cores, and Claude capture averaged only 0.12
  CPU cores versus 0.92 in the baseline. Ordered raw/seal digests, all 17 canonical tables,
  every asset and 8,000 measured ranked-search queries match; all source checksums were reverified.
  The affected suite passes 230 tests (two existing opt-in tests ignored), Clippy with warnings
  denied and formatting. Evidence and harnesses remain in `/private/tmp/rosetta-capture-opt-20260925/`;
  the [capture investigation](COLD_START_INVESTIGATION_2026-09-25.md#capture-investigation-and-ledger-optimization)
  records all timings, hashes and limitations. Derived output trees were removed after validation.
- Frozen campaign `apply-opt-20260925` uses the same read-only snapshot, checksummed query
  suite, M1 Max/64 GiB machine, and serial fresh release-serve configuration above. Cursor-local
  generation eviction replaces whole-buffer scans; borrowed fields and patches avoid repeated
  payload copies; cold session touches coalesce; tool activity bounds enter the initial insert.
  Session-touch SQL calls fall 397,583 → 10,574 for Claude and 48,849 → 568 for Codex.
  Ordered raw/seal digests, all 17 logical canonical tables, assets, and 8,000 measured ranked
  searches match, with source checksums reverified. Store/pipeline tests pass 240 cases (one
  opt-in ignored), and affected-crate all-target Clippy passes with warnings denied.
  Full readiness was 137.20 → 105.30 s / 77.62 → 61.77 s, but Claude apply itself was
  24.75 → 26.88 s and Codex 8.83 → 8.41 s under different host pressure: these results do
  not establish a code-only startup speedup. Binary/source hashes, stage/resource data,
  equality reports, and harnesses remain in `/private/tmp/rosetta-apply-opt-20260925/`;
  the [application investigation](COLD_START_INVESTIGATION_2026-09-25.md#applyreconciliation-optimization--2026-09-25)
  includes the later full live-history measurements and their limitations.
- Every `src_cursor` reconciliation predicate has a matching index. Query-plan tests pin
  these indexes because one missing provenance index turns source activation quadratic.
- A performance optimization is merge-admissible only with a frozen before/after pair
  from the same permission-read-only checksummed source snapshot for cold-write work or
  the same ledger for replay work, plus one checksummed query suite, campaign ID, machine,
  configuration, logical projection counts, and ranked-result digests. Estimates and
  unpaired organic runs remain planning evidence. The first search step is the FTS5
  p50/p95/p99 baseline; a Tantivy bake-off starts only if the declared or observed query
  mix misses the product objective or relevance/availability requires it.
- Memory-first fan-out for live streams (§5.2); DB commit is not on the notify path.
- Budgets asserted in CI-able bench: full `~/.claude` cold backfill < 5 s per GB of
  transcripts; watch-event → subscriber frame p99 < 20 ms; search p99 < 50 ms warm.

## 8. Workspace layout

```
rosetta/
├── Cargo.toml                # Rust workspace; every member is under crates/
├── CLAUDE.md                 # project rules for Claude sessions
├── rust-toolchain.toml       # pinned toolchain
├── docs/
│   ├── ARCHITECTURE.md       # this document (living)
│   └── groundtruth/
│       ├── RESEARCH_PLAN.md  # per-agent sourcing plans + survey (living)
│       └── PATTERNS.md       # Phase-2 cross-agent synthesis (future)
├── crates/
│   ├── adapter-support/      # permissive raw-parser + replay-test primitives
│   ├── core/                 # canonical model (hypothesis), ids, versions   (L2 heart)
│   ├── capture/              # watcher, tailers (tail-append JSONL / whole-file
│   │                         #   JSON / SQLite WAL), cursors, raw ledger     (L2 runtime)
│   ├── adapters/             # one production parser crate per captured agent
│   │   ├── claude-code/
│   │   └── codex/
│   ├── pipeline/             # ingest orchestration, analyzers, replay       (L2 runtime, Phase 3)
│   ├── store/                # canonical SQLite projection + search repair log (L2 persistence, Phase 3)
│   ├── search/               # detached FTS5 sidecar + publication worker   (L3 search boundary)
│   ├── protocol/             # wire types (serde + ts-rs), versions          (L3 contract, Phase 4)
│   ├── server/               # axum REST + WS services                       (L3 runtime, Phase 4)
│   ├── search-bench/         # frozen-input search percentile campaigns     (evidence)
│   └── rosettad/             # binary: daemon + CLI (run, backfill, probe,
│                             #   replay-diff, search, doctor)
├── groundtruth/              # DATA ONLY; one directory per roster agent
│   ├── claude-code/          # layout, sources, fixtures, canonical JSON+HTML
│   ├── codex/
│   └── …                     # watch.toml first; evidence added breadth-first
├── tools/groundtruth/        # stdlib Python evidence pipeline               (Phase 1)
├── clients/                  # generated consumer packages, e.g. typescript/ (Phase 4)
└── .github/workflows/        # ci.yml; release-watch.yml when automation is enabled
```

Dependency rule (enforced, not aspirational): `adapters → {core, adapter-support}`;
`pipeline → {core, capture, store, adapters}`; `server → {core, protocol, store}`;
**`core` and `protocol` depend on almost nothing**; nothing depends on `rosettad`.
Python tools read and write ground-truth data but are outside the Cargo graph.
Production crates never import development tooling or intermediate evidence.

## 9. Testing strategy

1. **Fixture corpus** per agent per version, replayed in CI (parse → normalize → golden).
2. **Differential replay**: new normalizer vs old over the full ledger, diff report.
3. **Tailer property tests**: random truncation/rewrite/partial-line torture.
4. **Contract goldens**: recorded observe/metrics WS sessions per protocol version.
5. **Probe in anger**: run `rosetta probe` against the live home dirs after every agent
   update; drift report drives fixture refresh.

## 10. Roadmap — layer by layer, breadth-first

Vertical feature slices built on one agent are how spaghetti overfit its canonical model
to Claude Code — every later adapter had to contort to fit. Rosetta is built **layer by
layer**, and each layer is validated across the whole agent roster before the next
layer's design is frozen.

- **Phase 1 — Ground truth (L1), across all mainstream agents.**
  Execute [groundtruth/RESEARCH_PLAN.md](groundtruth/RESEARCH_PLAN.md) over the roster
  (Claude Code, Codex, Grok, Antigravity, Copilot CLI, OpenCode, Kimi CLI, Hermes, Pi,
  …): per-agent layout manifests, versioned raw schemas, fixture farm, and release-watch
  automation.
  *Exit: every roster agent has a committed layout manifest, raw types, per-version
  fixtures, and green parse-replay in CI.*
- **Phase 2 — Pattern synthesis (L2 design).**
  A cross-agent comparison distills the common shapes (sessions, entries, tool calls,
  usage metrics, artifacts) and their per-agent variance; the §4 canonical model is
  revised from that evidence and ratified as v1, with per-agent mapping notes.
  *Exit: canonical model validated against the full roster's ground truth.*
- **Phase 3 — Runtime ingestion + normalizer (L2), across all agents.**
  Watcher/tailers, raw ledger, `normalize()` per adapter, analyzers, store
  projection, and differential replay.
  *Exit: the entire fixture corpus normalizes clean; goldens stable.*
- **Phase 4 — Service provider (L3).**
  Search, observe, metrics over the now-stable canonical store; generated TS clients;
  consumer migrations (agent-view app, chopsticks).
  *Exit: spaghetti decommissionable.*

## 11. Open decisions (defaults chosen, cheap to revisit)

| Decision | Default | Revisit when |
|---|---|---|
| FTS5 vs Tantivy | detached FTS5 sidecar today; Tantivy prototype remains benchmark-only | revisit with judged relevance + declared/observed query mix + accepted storage margin; FTS5 coherence/crash gates are already implemented |
| WS vs SSE for streams | WS (bidirectional subscribe/ack) | if consumers end up read-only-simple |
| Ledger retention | keep forever; segments zstd-compressed since 2026-09-04 (`ledger compact` for older ones); no segment GC yet | compressed ledger > tens of GB |
| Image-member retention | keep immutable content-addressed members (no GC yet) | `.assets/` growth justifies a ledger-derived mark/sweep under the projection lock |
| Grok/Codex priority | after M2 | chopsticks needs them sooner |
| UDS transport | not in v1 | localhost TCP shows up in profiles |
| Enrolled-set live mutation | the `--enroll` file is read once at start; the supervisor restarts the daemon when a persona is added or retired, or when a kind is checked or UN-checked — the restart re-derives consent from the file in both directions, including out of a pending cold generation (2026-09-12) | a restart per persona change costs the app a visible reconnect, or a persona is created faster than a restart settles |
| Usage outside transcripts | requests an agent never records (Claude Code background title/summary calls) are out of coverage and labelled so (2026-09-22) | an agent records them somewhere Rosetta already captures |
| Aggregate samples on tiered models | a counted aggregate is estimated as equal requests at its average prompt (2026-09-23); an uncounted one prices only when its prompt tokens cannot have crossed a tier start, otherwise it is unpriced (2026-09-22) | a producer starts reporting per-request usage, or an uncounted producer exposes a count |
| Residency and regional premiums | a `route` qualifier prices only through a cited overlay premium; otherwise the record is unpriced (2026-09-22) | such routes appear in real data |
| Rediscover without restart | roll-call discovery runs once at start; a kind installed later is seen on the next start (2026-09-12) | the app wants a "rescan" affordance, or the supervisor needs to enroll into a running daemon |
