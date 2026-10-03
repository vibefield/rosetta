// The session tree, as the document needs it.
//
// Child sessions are document STRUCTURE, not sibling navigation: a proven
// `Delegate` link places a subthread beneath its own ledger line, and a
// parent-only link is interleaved by session start time without pretending
// that timestamp proximity proves cause.
import type { SessionRow } from "@rosetta/protocol";

export interface ThreadFocus {
  readonly sessionId: string;
  readonly timelineId: string;
}

export interface ThreadIndex {
  readonly byId: ReadonlyMap<string, SessionRow>;
  readonly childrenByParent: ReadonlyMap<string, readonly SessionRow[]>;
}

export function buildThreadIndex(sessions: readonly SessionRow[]): ThreadIndex {
  const byId = new Map(sessions.map((session) => [session.id, session]));
  const childrenByParent = new Map<string, SessionRow[]>();
  for (const session of sessions) {
    if (session.parent_session_id === null) continue;
    const children = childrenByParent.get(session.parent_session_id);
    if (children) children.push(session);
    else childrenByParent.set(session.parent_session_id, [session]);
  }
  for (const children of childrenByParent.values()) {
    children.sort(
      (left, right) => left.started_at - right.started_at || left.id.localeCompare(right.id),
    );
  }
  return { byId, childrenByParent };
}

export function sessionContains(
  ancestorId: string,
  candidateId: string,
  sessions: ReadonlyMap<string, SessionRow>,
): boolean {
  let current: string | null = candidateId;
  const visited = new Set<string>();
  while (current !== null && !visited.has(current)) {
    if (current === ancestorId) return true;
    visited.add(current);
    current = sessions.get(current)?.parent_session_id ?? null;
  }
  return false;
}

/** The element id a row's element carries. Search results land on it. */
export function messageDomId(sessionId: string, timelineId: string): string {
  return `msg-${sessionId}-${timelineId}`;
}

export function subthreadTitle(session: SessionRow): string {
  if (session.title !== null) return session.title;
  if (session.summary !== null) return session.summary;
  const native = session.native_session_id.split("/").at(-1) ?? session.native_session_id;
  return `Subagent ${native.length > 12 ? native.slice(-12) : native}`;
}
