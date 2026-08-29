# Hand-off: progressive first-run publication (cold-start slice 3)

**To:** the L2 (pipeline/store) owner. **From:** the L3/app session, 2026-08-29.
**Status:** open ask — the last unlanded slice of the cold-start design.
**Full design record:** https://claude.ai/code/artifact/12eb71b1-9d56-47f9-9757-abf7f47e79a7
("The Cold Start" — states, veil laws, wire contract, screenshots of every landed state).

This is a transient hand-off brief, not a third living document — delete it when
slice 3 lands (per the two-living-docs discipline in CLAUDE.md).

## Where things stand

Slices 1, 2, 4, 5 are on `main` and live-verified:

| Slice | What | Commits |
|---|---|---|
| 1 | Serve binds the listener at process start; `/api/status` answers with no projection file; `DaemonStatus.ingest` meters from shared `Arc<Instrument>` counters; hub mirror re-warms every 2s during backfill and fans diffs to `/ws/meta` | `37e3459` |
| 2 | App veil system: readiness derived from `SessionRow.stream_state` + ingest phase; cooking cards; empty-vs-pending law; fresh-install warming room | `913b734` |
| 4 | Consent roll call: `GET /api/discoveries`, one-shot `POST /api/runtimes/enable`, phase `awaiting_consent`; the projection's runtime `enabled` flags ARE the consent record; `--agent`/`--all-agents` for headless runs | `192ac9d`, `d54fe26` |
| 5 | Boost: serve drives the stream queue **one file per `stream_backfill_into` call** with `audit_files_per_run: 0`, plus one audit-enabled full-roster pass at the end; `POST /api/ingest/boost {session_id}` drains between files → front of queue; the app boosts automatically on selecting a pending session | `192ac9d`, `d54fe26` |

## The gap

On a **fresh install** (no `~/.rosetta/proj.db`), serve still cold-loads into a
private temp projection and publishes atomically at the end
(`open_for_backfill` in `crates/rosettad/src/main.rs` returns a temp path when
the db file is missing; `publish_private_projection` swaps it in). So the one
run where progressive reveal matters most shows: roll call → warming panel with
meters → everything at once. The per-session pending→indexed reveal the app
already renders only shows on existing-db restarts, where backfill writes in
place.

## The ask

Make the first-run backfill write the **public** projection, per-file, with FTS
deferred:

1. **Per-file public commits.** The unit of publication already exists: serve's
   stream loop calls `stream_backfill_into` once per file
   (`crates/rosettad/src/serve.rs`, the `VecDeque` driver around line 357).
   This slice is essentially "point that loop at the public projection instead
   of the private temp, and defer FTS."
2. **FTS deferred to an async search worker.** Cold-load currently defers FTS/
   rollups/indexes until publication; keep that win by landing the
   `search_dirty` queue **with its consumer in the same commit** (the repo
   convention already reserves exactly this pair: `projection_state` /
   `search_dirty` do not land before the asynchronous search worker that
   consumes and acknowledges them). Search readiness is already a separate,
   surfaced fact on the wire — the app treats "listed and readable" and
   "searchable" independently.
3. **`rebuild` stays atomic and private.** It is a repair tool; correctness
   over liveness. Only the fresh-install ingest/watch path goes progressive.
4. **Ordering: interleave runtimes round-robin, newest-first within each.**
   5,912 claude-code sessions must not starve grok's 131. (Today serve runs
   runtimes sequentially; boost only reorders within the current runtime's
   queue — round-robin also fixes that.)

### What must not regress

- The consent gate blocks **before** any read; the boost ledger drains between
  files; ctrl-c lands between files (interrupted runs currently skip publish —
  decide what "interrupted" means once commits are public; the ledger +
  checkpoints already make resume correct).
- Generation-safe rewrites, deterministic adapter checkpoints, the checksummed
  raw ledger, and the single-writer topology (hub absorbs drained effects
  strictly after commit) all stay as they are.
- The audit lane: per-file slices must keep `audit_files_per_run: 0` —
  `rotating_audit_paths` picks from the passed slice, so 1-file slices would
  force-replay everything on restart. One audit-enabled full-roster pass at
  the end preserves the single rotating forced replay.

## Acceptance gates

Judged per the repo's perf convention — a frozen before/after pair on the same
checksummed source snapshot via `tools/ingest-bench`, logical counts and
ranked-result digests equal:

- **time-to-lists < 10 s** (meta skim visible through the public projection)
- **time-to-first-readable-session < 15 s**
- **total wall time ≤ 1.5× the private atomic path** — if this gate misses,
  batch N files per transaction until it doesn't.

Open question (owner: James): is 1.5× the right ceiling, given time-to-useful
falls from ~5 minutes to ~10 seconds?

## How the app will see it (nothing for L2 to build here)

Already live, keyed only off wire facts: `/api/status` ingest meters, the 2s
mirror re-warm fanning to `/ws/meta`, and `stream_state` flips per session.
The moment first-run commits are public, the existing veils/cooking-card UI
lights up progressively with zero app changes. Smoke recipe for photographing
mid-backfill states (staged growing fixtures, debug backfill ~5k rows/s) is in
the design record.
