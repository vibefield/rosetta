import type { TimelineMeta, TimelineMetaTask, TimelineMetaWorkflow } from "@vibefield/rosetta-protocol";

export const TIMELINE_META_KINDS = ["tasks", "workflows", "plans", "artifacts"] as const;
export type TimelineMetaKind = (typeof TIMELINE_META_KINDS)[number];

/** Archived running states do not prove work continued until the wall clock. */
export function recordedTaskDuration(task: TimelineMetaTask): number | null {
  if (task.status === "in_progress" || task.started_at === null || task.terminal_at === null) return null;
  const duration = task.terminal_at - task.started_at;
  return Number.isFinite(duration) && duration >= 0 ? duration : null;
}

export function recordedWorkflowDuration(workflow: TimelineMetaWorkflow): number | null {
  const duration = workflow.duration_ms;
  return duration !== null && Number.isFinite(duration) && duration >= 0 ? duration : null;
}

/** Empty categories are not navigation: keep both their launcher and tab out
 * of the focus order, and omit the entire control when the session has none. */
export function availableTimelineMetaKinds(meta: TimelineMeta): TimelineMetaKind[] {
  return TIMELINE_META_KINDS.filter((kind) => (meta[kind] ?? []).length > 0);
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
