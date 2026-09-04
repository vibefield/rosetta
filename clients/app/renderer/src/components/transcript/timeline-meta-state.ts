import type { TimelineMeta } from "@rosetta/protocol";

export const TIMELINE_META_KINDS = ["tasks", "plans", "artifacts"] as const;
export type TimelineMetaKind = (typeof TIMELINE_META_KINDS)[number];

/** Empty categories are not navigation: keep both their launcher and tab out
 * of the focus order, and omit the entire control when the session has none. */
export function availableTimelineMetaKinds(meta: TimelineMeta): TimelineMetaKind[] {
  return TIMELINE_META_KINDS.filter((kind) => meta[kind].length > 0);
}

/** Live replacement frames can remove the open category. Keep the island on
 * the first remaining category; close it only when no session work remains. */
export function resolveTimelineMetaKind(
  active: TimelineMetaKind | null,
  available: readonly TimelineMetaKind[],
): TimelineMetaKind | null {
  if (active === null || available.includes(active)) return active;
  return available[0] ?? null;
}
