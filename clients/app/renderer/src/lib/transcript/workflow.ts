// Workflow presentation facts. Attempts are chronological timeline evidence;
// the run is a separately hydrated, mutable document. Keeping this join here
// prevents React from guessing lifecycle from prose or duplicating workers.
import type {
  TimelineEntry,
  ToolStatus,
  WorkflowRunStatus,
  WorkflowWorker,
} from "@rosetta/protocol";
import type { WorkflowAction } from "./families.ts";

export interface WorkflowAttemptFact {
  readonly timelineId: string;
  readonly attempt: number;
  readonly taskId: string | null;
  readonly runId: string | null;
  readonly resumed: boolean;
  readonly status: ToolStatus;
  readonly at: number | null;
  readonly completedAt: number | null;
  readonly durationMs: number | null;
  readonly summary: string | null;
}

export interface WorkflowAttemptIndex {
  readonly byRunId: ReadonlyMap<string, readonly WorkflowAttemptFact[]>;
  readonly byTimelineId: ReadonlyMap<string, WorkflowAttemptFact>;
}

function attemptFact(row: TimelineEntry, action: WorkflowAction): WorkflowAttemptFact {
  const timing = row.body.type === "tool_use" ? row.body.tool.timing : null;
  const status = row.body.type === "tool_use" ? row.body.tool.status.value : "unknown";
  const at = timing?.started_at ?? row.at;
  const completedAt = timing?.completed_at ?? null;
  return {
    timelineId: row.timeline_id,
    attempt: action.attempt ?? 1,
    taskId: action.task_id,
    runId: action.run_id,
    resumed: (action.attempt ?? 1) > 1 || action.resume_id !== null,
    status,
    at,
    completedAt,
    durationMs:
      timing?.duration_ms ??
      (at !== null && completedAt !== null && completedAt >= at ? completedAt - at : null),
    summary: action.callback_summary,
  };
}

export function workflowAttemptIndex(rows: readonly TimelineEntry[]): WorkflowAttemptIndex {
  const mutableByRunId = new Map<string, WorkflowAttemptFact[]>();
  const byTimelineId = new Map<string, WorkflowAttemptFact>();
  for (const row of rows) {
    if (row.body.type !== "tool_use" || row.body.tool.action.type !== "workflow") continue;
    const action = row.body.tool.action;
    const fact = attemptFact(row, action);
    byTimelineId.set(row.timeline_id, fact);
    if (action.run_id === null) continue;
    const attempts = mutableByRunId.get(action.run_id);
    if (attempts === undefined) mutableByRunId.set(action.run_id, [fact]);
    else attempts.push(fact);
  }
  const byRunId = new Map<string, readonly WorkflowAttemptFact[]>();
  for (const [runId, attempts] of mutableByRunId) {
    byRunId.set(
      runId,
      attempts.sort((left, right) => left.at === right.at ? left.attempt - right.attempt : (left.at ?? Number.MAX_SAFE_INTEGER) - (right.at ?? Number.MAX_SAFE_INTEGER)),
    );
  }
  return { byRunId, byTimelineId };
}

export function workflowAttemptsFor(
  index: WorkflowAttemptIndex,
  row: TimelineEntry,
): readonly WorkflowAttemptFact[] {
  if (row.body.type !== "tool_use" || row.body.tool.action.type !== "workflow") return [];
  const runId = row.body.tool.action.run_id;
  if (runId !== null) return index.byRunId.get(runId) ?? [];
  const attempt = index.byTimelineId.get(row.timeline_id);
  return attempt === undefined ? [] : [attempt];
}

export interface WorkflowRuntimeFact {
  readonly basis: "observed" | "varies" | "configured" | "unknown";
  readonly model: string;
  readonly effort: string;
}

/** Observed child-response metadata wins. A sidecar configuration is a
 * labelled fallback, never presented as observation. */
export function workflowRuntime(
  worker: WorkflowWorker,
  defaultModel: string | null,
): WorkflowRuntimeFact {
  const observedModels = [...new Set(worker.observed_models ?? [])];
  const observedEfforts = [...new Set(worker.observed_efforts ?? [])];
  if (observedModels.length > 1 || observedEfforts.length > 1) {
    return {
      basis: "varies",
      model: observedModels.length > 1 ? `varies (${observedModels.length})` : (observedModels[0] ?? "model varies"),
      effort: observedEfforts.length > 1 ? `varies (${observedEfforts.length})` : (observedEfforts[0] ?? "unknown"),
    };
  }
  if (observedModels.length === 1 || observedEfforts.length === 1) {
    return {
      basis: "observed",
      model: observedModels[0] ?? "unknown",
      effort: observedEfforts[0] ?? "unknown",
    };
  }
  const model = worker.configured_model ?? defaultModel;
  const effort = worker.configured_effort;
  if (model !== null || effort !== null) {
    return {
      basis: "configured",
      model: model ?? "unknown",
      effort: effort ?? "unknown",
    };
  }
  return { basis: "unknown", model: "unknown", effort: "unknown" };
}

/** Terminal run state refines unfinished worker rows without falsifying the
 * native worker outcome. Cached completion is called reused and claims no
 * fresh accounting. */
export function workflowWorkerState(
  worker: WorkflowWorker,
  runStatus: WorkflowRunStatus,
): string {
  if (worker.cached && worker.state === "done") return "reused";
  if (["killed", "cancelled", "stopped"].includes(runStatus)) {
    if (worker.state === "progress") return "interrupted";
    if (worker.state === "start") return "never started";
  }
  switch (worker.state) {
    case "done":
      return "done";
    case "error":
      return "error";
    case "progress":
      return "running";
    case "start":
      return "queued";
    case "unknown":
      return "status unknown";
  }
}
