import type { SessionRow } from "@vibefield/rosetta-protocol";

/** Recorded conversation time; metadata/file timestamps are never a fallback. */
export function sessionLastActive(session: SessionRow): number | null {
  const at = session.activity?.last_at;
  return typeof at === "number" && Number.isFinite(at) ? at : null;
}

/** Newest known conversation first, then a stable id; unknowns go last. */
export function compareSessionActivity(left: SessionRow, right: SessionRow): number {
  const a = sessionLastActive(left);
  const b = sessionLastActive(right);
  if (a === null && b !== null) return 1;
  if (b === null && a !== null) return -1;
  return (a !== null && b !== null ? b - a : 0) || left.id.localeCompare(right.id);
}
