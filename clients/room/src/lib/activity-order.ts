import type { SessionRow } from "@vibefield/rosetta-protocol";
import { sessionLastActive } from "./session-activity";

/** A quiet interval rearms promotion. Its expiry never changes the order. */
export const ACTIVITY_QUIET_MS = 5 * 60_000;

interface ActivityPosition {
  readonly seenAt: number | null;
  readonly rankAt: number | null;
}

interface SessionPosition extends ActivityPosition {
  readonly projectId: string | null;
  readonly parentId: string | null;
}

export interface ActivityOrder {
  readonly sessions: ReadonlyMap<string, SessionPosition>;
  readonly projects: ReadonlyMap<string | null, ActivityPosition>;
  readonly bootstrapping: boolean;
}

export interface ActivityOrderOptions {
  readonly now: number;
  /** False for snapshots, reconnection, and indexing; those are baseline data. */
  readonly live: boolean;
  readonly bootstrapping: boolean;
}

function latest(a: number | null, b: number | null): number | null {
  return a === null ? b : b === null ? a : Math.max(a, b);
}

function fresh(at: number, now: number): boolean {
  return at <= now && now - at < ACTIVITY_QUIET_MS;
}

function afterQuiet(at: number, previous: number | null): boolean {
  return previous === null || at - previous >= ACTIVITY_QUIET_MS;
}

/** Client-only navigation memory. Recorded activity is never rewritten.
 * Observe every session, including children, so reparenting is not new work.
 * Project progress is folded across roots before deciding whether it was quiet. */
export function advanceActivityOrder(
  previous: ActivityOrder | null,
  rows: readonly SessionRow[],
  options: ActivityOrderOptions,
): ActivityOrder {
  const seed = previous === null || options.bootstrapping || previous.bootstrapping;
  const live = options.live && !seed;
  const sessions = new Map<string, SessionPosition>();
  const projectActivity = new Map<string | null, number | null>();
  const advances = new Map<string | null, number[]>();

  for (const row of rows) {
    const at = sessionLastActive(row);
    const old = previous?.sessions.get(row.id);
    const sameOwner = old === undefined ||
      (old.projectId === row.project_id && old.parentId === row.parent_session_id);
    const advanced = at !== null && (old?.seenAt == null || at > old.seenAt);
    const promote = live && sameOwner && advanced && fresh(at, options.now) &&
      afterQuiet(at, old?.seenAt ?? null);
    sessions.set(row.id, {
      projectId: row.project_id,
      parentId: row.parent_session_id,
      // A repair may retract activity. It must not rearm promotion on replay.
      seenAt: seed ? at : latest(old?.seenAt ?? null, at),
      rankAt: seed || old === undefined || old.rankAt === null || promote ? at : old.rankAt,
    });
    if (row.parent_session_id !== null) continue;
    projectActivity.set(row.project_id, latest(projectActivity.get(row.project_id) ?? null, at));
    if (live && sameOwner && advanced) {
      const events = advances.get(row.project_id) ?? [];
      events.push(at);
      advances.set(row.project_id, events);
    }
  }

  const projects = new Map<string | null, ActivityPosition>();
  for (const [id, at] of projectActivity) {
    const old = previous?.projects.get(id);
    let rankAt = seed || old === undefined || old.rankAt === null ? at : old.rankAt;
    let seenAt = seed ? at : old?.seenAt ?? null;
    // Ascending order prevents coalesced progress from manufacturing a lull
    // between a project's previous event and the newest event in this batch.
    for (const event of (advances.get(id) ?? []).sort((a, b) => a - b)) {
      if (fresh(event, options.now) && afterQuiet(event, seenAt)) rankAt = event;
      seenAt = latest(seenAt, event);
    }
    projects.set(id, { seenAt: latest(seenAt, at), rankAt });
  }
  return { sessions, projects, bootstrapping: options.bootstrapping };
}

/** Known timestamps precede unknowns; identity deterministically breaks ties. */
export function compareActivityRanks(
  a: number | null, b: number | null, aId: string, bId: string,
): number {
  if (a === null && b !== null) return 1;
  if (b === null && a !== null) return -1;
  return (a !== null && b !== null ? b - a : 0) || aId.localeCompare(bId);
}

/** Local calendar day, rather than a 24-hour duration (including across DST). */
export function activityDay(at: number | null): number | null {
  if (at === null) return null;
  const date = new Date(at);
  date.setHours(0, 0, 0, 0);
  return Number.isFinite(date.getTime()) ? date.getTime() : null;
}

export function compareOrderedSessions(
  left: SessionRow, right: SessionRow, order: ActivityOrder,
): number {
  const aDay = activityDay(sessionLastActive(left));
  const bDay = activityDay(sessionLastActive(right));
  // Calendar groups remain truthful, including a conversation continuing
  // across midnight. Within each day only a new activity period can promote.
  if (aDay !== bDay) return compareActivityRanks(aDay, bDay, left.id, right.id);
  return compareActivityRanks(
    order.sessions.get(left.id)?.rankAt ?? null,
    order.sessions.get(right.id)?.rankAt ?? null,
    left.id, right.id,
  );
}
