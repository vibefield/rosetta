// The cold start, read off the wire: /api/status's ingest block turned into
// the few facts the room asks about it. Readiness is data, never a timer —
// every veil and cooking affordance keys off these derivations plus
// SessionRow.stream_state, and nothing else. A daemon too old to carry the
// block yields null throughout, which degrades to today's behavior.
import type { IngestProgress, ProjectBackfill, SessionRow } from "@vibefield/rosetta-protocol";
import { UNASSIGNED_PROJECT_ID } from "./model";

/** The backfill is still running: veils may be down, cards may cook. */
export function isIngestActive(ingest: IngestProgress | null): boolean {
  return ingest !== null && ingest.phase !== "live";
}

/** Keep resume visible if discovery reaches the consent gate while paused.
 * Once resumed, the host's ordinary consent screen can take over again. */
export function isAwaitingConsent(ingest: IngestProgress | null): boolean {
  return ingest?.phase === "awaiting_consent" &&
    (ingest.run == null || ingest.run.state === "running");
}

/** The census (skim) has not yet named every session: the LISTS themselves
 * are still incomplete, so the list sections stay veiled. */
export function isCensusRunning(ingest: IngestProgress | null): boolean {
  if (!isIngestActive(ingest)) return false;
  if (ingest === null) return true;
  // A completed first pass stays complete while queued runtimes start
  // streaming or later work changes phase. Older daemons omit the stamp.
  if (ingest.metadata_ready_at !== undefined) return ingest.metadata_ready_at === null;
  // The explicit audit seam proves every bounded skim has committed. Runtime
  // rows remain queued because complete-record streaming has not started.
  if (ingest.phase === "metadata_ready") return false;
  if (ingest.runtimes.length === 0) return true;
  return ingest.runtimes.some(
    (runtime) => runtime.state === "queued" || runtime.state === "skimming",
  );
}

/** Census meter: files skimmed over the discovery walk's total. */
export function censusProgress(ingest: IngestProgress | null): number | null {
  if (ingest === null) return null;
  const totals = ingest.runtimes.reduce(
    (sum, runtime) => ({
      done: sum.done + runtime.files_skimmed,
      all: sum.all + runtime.files_total,
    }),
    { done: 0, all: 0 },
  );
  return totals.all > 0 ? totals.done / totals.all : null;
}

/** Stream meter: transcript bytes read over the census's byte total. */
export function backfillProgress(ingest: IngestProgress | null): number | null {
  if (ingest === null) return null;
  const totals = ingest.runtimes.reduce(
    (sum, runtime) => ({
      done:
        sum.done +
        (runtime.state === "done" ? runtime.bytes_total : runtime.bytes_streamed),
      all: sum.all + runtime.bytes_total,
    }),
    { done: 0, all: 0 },
  );
  return totals.all > 0 ? Math.min(1, totals.done / totals.all) : null;
}

/** A session whose figures are still cooking. During the backfill this is
 * the cooking card; during live it marks a transcript still being read. */
export function isSessionCooking(session: SessionRow): boolean {
  return session.stream_state !== "indexed";
}

/** The daemon's per-project backfill fold, keyed the way the room keys
 * projects — the canonical id, with the daemon's unassigned bucket under
 * UNASSIGNED_PROJECT_ID. Empty until the skims settle the plan, and empty
 * on a daemon too old to carry the fold, so every consumer degrades to the
 * meta-derived cooking state it had before. */
export function projectBackfills(
  ingest: IngestProgress | null,
): ReadonlyMap<string, ProjectBackfill> {
  const index = new Map<string, ProjectBackfill>();
  for (const row of ingest?.projects ?? []) {
    index.set(row.project_id ?? UNASSIGNED_PROJECT_ID, row);
  }
  return index;
}

/** One project's byte meter, 0..1. A source that grew mid-backfill can
 * overshoot its census total, so the meter clamps; null when the census
 * counted nothing to read. */
export function projectBackfillProgress(row: ProjectBackfill): number | null {
  if (row.bytes_total === 0) return null;
  return Math.min(1, row.bytes_streamed / row.bytes_total);
}

/** "2,314 of 5,912 sessions" — the veil labels' one counting form. */
export function sessionCountLabel(indexed: number, total: number): string {
  return `${indexed.toLocaleString()} of ${total.toLocaleString()} sessions`;
}
