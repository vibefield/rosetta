// View models: rosetta wire rows → what the room draws. Pure functions of
// their inputs; every derived figure states its window at the callsite.
import type {
  AttachmentRef,
  CanonicalText,
  CitationRef,
  ContributionRow,
  HeatmapDay,
  Notice,
  ProjectRow,
  ResourceRef,
  SessionRow,
  TimelineEntry,
  ToolAction,
} from "@rosetta/protocol";
import type { AgentShare } from "./agents";
import { projectTint } from "./tint";

export interface ProjectStats {
  /** Daily token totals over the heatmap's window (53 Sunday-aligned weeks
   * ending today), from /heatmap?since_ms=…. */
  heat: readonly HeatmapDay[];
  /** Per-root-session contribution (tokens, messages, …), whole project life. */
  contribution: readonly ContributionRow[];
}

export interface ProjectViewModel {
  readonly row: ProjectRow;
  readonly tint: string;
  /** Tokens over the last 30 days — the ledger's stated window. */
  readonly tokensLast30Days: number;
  /** 15 two-day buckets, oldest first — the activity shape. */
  readonly recentTokenBuckets: readonly number[];
  /** 53×7 daily totals from the grid's Sunday; cells after today are -1.
   * The heatmap is all of it and the 30-day figures are its tail, so the
   * charts are views of one number and cannot disagree. */
  readonly dailyTokens: readonly number[];
  /** Index of today inside dailyTokens. */
  readonly currentDayIndex: number;
  /** Tokens over the whole 53-week window — the summary panel's stated window. */
  readonly tokensLast53Weeks: number;
  /** Message rows across the project's whole life (contribution sum). */
  readonly lifetimeMessages: number;
  readonly agents: readonly AgentShare[];
  readonly lastActiveMs: number | null;
  readonly rootSessionCount: number;
  /** Every session of the project, subagents included — the readiness base. */
  readonly sessionCount: number;
  /** Sessions the stream phase has fully indexed. */
  readonly indexedSessionCount: number;
  /** Folded over the sessions' stream_state: ready = every figure on this
   * project is final. Readiness is data, never a timer. */
  readonly readiness: "pending" | "partial" | "ready";
}

const DAY_IN_MS = 86_400_000;

/** Client-only bucket for sessions whose adapters have no native cwd fact.
 * It is deliberately not persisted as a canonical project. */
export const UNASSIGNED_PROJECT_ID = "__rosetta_unassigned__";

export function projectKey(session: SessionRow): string {
  return session.project_id ?? UNASSIGNED_PROJECT_ID;
}

export function unassignedProject(): ProjectRow {
  return {
    id: UNASSIGNED_PROJECT_ID,
    name: "Unassigned sessions",
    root_paths: [],
    git_repo: null,
  };
}

/* ── heatmap grid: 53 columns of weeks, Sunday-aligned, ending today ────── */

export const ACTIVITY_HEATMAP_WEEK_COUNT = 53;
export const ACTIVITY_HEATMAP_DAY_COUNT = ACTIVITY_HEATMAP_WEEK_COUNT * 7;

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Midnight of the Sunday opening the grid's first column. */
export function getActivityGridStartTimestamp(currentTimestamp: number): number {
  const today = startOfDay(currentTimestamp);
  return (
    today -
    ((ACTIVITY_HEATMAP_WEEK_COUNT - 1) * 7 + new Date(today).getDay()) * DAY_IN_MS
  );
}

/** The viewer's offset, minutes east of UTC — passed to /heatmap so the
 * daemon buckets days the way this screen will label them. */
export function getViewerUtcOffsetMinutes(): number {
  return -new Date().getTimezoneOffset();
}

/** The viewer-offset day key for a timestamp — the exact form the heatmap
 * endpoint was asked to bucket in. */
export function getCalendarDayKey(timestamp: number): string {
  const d = new Date(timestamp + getViewerUtcOffsetMinutes() * 60_000);
  return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, "0")}-${String(
    d.getUTCDate(),
  ).padStart(2, "0")}`;
}

/** The full 53×7 grid, zero-filled from sparse heatmap days; cells after
 * today hold -1 (not yet, which is not the same as empty). */
function createActivitySeries(
  heatmapDays: readonly HeatmapDay[],
  currentTimestamp: number,
): { dailyTokens: number[]; currentDayIndex: number } {
  const tokensByDay = new Map(heatmapDays.map((day) => [day.day, day.tokens]));
  const startTimestamp = getActivityGridStartTimestamp(currentTimestamp);
  const currentDayKey = getCalendarDayKey(currentTimestamp);
  const dailyTokens: number[] = [];
  let currentDayIndex = ACTIVITY_HEATMAP_DAY_COUNT - 1;
  for (let index = 0; index < ACTIVITY_HEATMAP_DAY_COUNT; index += 1) {
    const key = getCalendarDayKey(startTimestamp + index * DAY_IN_MS + DAY_IN_MS / 2);
    if (key === currentDayKey) currentDayIndex = index;
    dailyTokens.push(index > currentDayIndex ? -1 : (tokensByDay.get(key) ?? 0));
  }
  return { dailyTokens, currentDayIndex };
}

export function buildProjectViewModel(
  projectRow: ProjectRow,
  allSessions: readonly SessionRow[],
  projectStats: ProjectStats | undefined,
  getRuntimeKind: (runtimeId: string) => string,
  currentTimestamp: number,
): ProjectViewModel {
  const projectSessions = allSessions.filter(
    (session) => projectKey(session) === projectRow.id,
  );
  const rootSessions = projectSessions.filter(
    (session) => session.id === session.root_session_id,
  );
  const indexedSessionCount = projectSessions.filter(
    (session) => session.stream_state === "indexed",
  ).length;
  const readiness =
    indexedSessionCount === projectSessions.length
      ? "ready"
      : indexedSessionCount === 0
        ? "pending"
        : "partial";
  const { dailyTokens, currentDayIndex } = createActivitySeries(
    projectStats?.heat ?? [],
    currentTimestamp,
  );
  // The ledger's 30-day figures are the same series' tail, so the heatmap,
  // the sparkline, and the figure can never disagree.
  const dailyTokensLast30Days = Array.from({ length: 30 }, (_, dayIndex) =>
    Math.max(0, dailyTokens[currentDayIndex - (29 - dayIndex)] ?? 0),
  );
  const recentTokenBuckets = Array.from({ length: 15 }, (_, bucketIndex) =>
    [dailyTokensLast30Days[bucketIndex * 2], dailyTokensLast30Days[bucketIndex * 2 + 1]].reduce(
      (total, value) => total + (value ?? 0),
      0,
    ),
  );
  const tokensLast30Days = dailyTokensLast30Days.reduce((total, value) => total + value, 0);
  const tokensLast53Weeks = dailyTokens.reduce(
    (total, value) => total + Math.max(0, value),
    0,
  );
  const lifetimeMessages = (projectStats?.contribution ?? []).reduce(
    (total, contribution) => total + contribution.messages,
    0,
  );

  const sessionCountByRuntimeKind = new Map<string, number>();
  for (const session of rootSessions) {
    const runtimeKind = getRuntimeKind(session.runtime_id);
    sessionCountByRuntimeKind.set(
      runtimeKind,
      (sessionCountByRuntimeKind.get(runtimeKind) ?? 0) + 1,
    );
  }
  const totalSessionCount = rootSessions.length || 1;
  const agents: AgentShare[] = [...sessionCountByRuntimeKind.entries()]
    .map(([kind, count]) => ({
      kind,
      pct: Math.round((count / totalSessionCount) * 100),
    }))
    .sort((left, right) => right.pct - left.pct);

  const lastActiveMs = rootSessions.length
    ? Math.max(...rootSessions.map((session) => session.updated_at))
    : null;
  return {
    row: projectRow,
    tint: projectTint(projectRow.id),
    tokensLast30Days,
    recentTokenBuckets,
    dailyTokens,
    currentDayIndex,
    tokensLast53Weeks,
    lifetimeMessages,
    agents,
    lastActiveMs,
    rootSessionCount: rootSessions.length,
    sessionCount: projectSessions.length,
    indexedSessionCount,
    readiness,
  };
}

/* ── the work block: a contiguous run of tool calls is ONE object ────────── */

export interface WorkCall {
  row: TimelineEntry;
  verb: string;
  target: string;
  result: string;
  failed: boolean;
}

export type DocBlock =
  | { type: "prose"; row: TimelineEntry }
  | { type: "aside"; row: TimelineEntry }
  | { type: "work"; key: string; calls: WorkCall[]; durMs: number | null };

function firstLine(text: string, max = 90): string {
  const line = text.split("\n", 1)[0].trim();
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

export function canonicalText(text: CanonicalText): string {
  return text.type === "inline" ? text.text : text.preview;
}

export function resourceLabel(resource: ResourceRef): string {
  switch (resource.type) {
    case "file":
    case "directory":
      return resource.path;
    case "url":
      return resource.url;
    case "session":
    case "task":
    case "process":
      return resource.id;
    case "asset":
      return resource.asset.url;
  }
}

export function attachmentLabel(attachment: AttachmentRef): string {
  const target = resourceLabel(attachment.source);
  const label = attachment.name?.trim() || target;
  const size = attachment.byte_len === null ? "" : ` · ${attachment.byte_len.toLocaleString()} bytes`;
  return `${attachment.kind}: ${label}${size}`;
}

export function citationLabel(citation: CitationRef): string {
  const source = citation.source ? resourceLabel(citation.source) : null;
  return citation.title?.trim() || source || citation.provider?.trim() || "Source";
}

export function noticeText(notice: Notice): string {
  switch (notice.type) {
    case "compaction":
      return notice.reason ? `Context compacted · ${notice.reason}` : "Context compacted";
    case "truncation": {
      const count = notice.omitted_count === null ? "" : ` · ${notice.omitted_count} omitted`;
      return `${notice.scope.replaceAll("_", " ")} truncated${count}`;
    }
    case "model_changed":
      return notice.from ? `Model changed from ${notice.from} to ${notice.to}` : `Model changed to ${notice.to}`;
    case "mode_changed":
      return notice.from ? `Mode changed from ${notice.from} to ${notice.to}` : `Mode changed to ${notice.to}`;
    case "permission_requested":
      return notice.scope ? `Permission requested · ${notice.scope}` : "Permission requested";
    case "permission_decision":
      return notice.scope ? `Permission ${notice.decision} · ${notice.scope}` : `Permission ${notice.decision}`;
    case "handoff":
      return `Handed off to ${notice.to_agent ?? notice.to_session_id ?? "another agent"}`;
    case "runtime_error":
      return `${notice.kind.replaceAll("_", " ")} · ${notice.summary}`;
  }
}

export function timelineText(row: TimelineEntry): string | null {
  switch (row.body.type) {
    case "user":
    case "agent":
      return row.body.text ? canonicalText(row.body.text) : null;
    case "thinking":
    case "summary":
      return canonicalText(row.body.text);
    case "notice":
      return noticeText(row.body.notice);
    case "tool_use":
      return null;
  }
}

export function timelineAttachments(row: TimelineEntry): readonly AttachmentRef[] {
  return row.body.type === "user" || row.body.type === "agent" ? row.body.attachments : [];
}

export function timelineCitations(row: TimelineEntry): readonly CitationRef[] {
  return row.body.type === "agent" ? row.body.citations : [];
}

function actionTarget(action: ToolAction): string {
  switch (action.type) {
    case "file_read":
      return action.targets.map((target) => resourceLabel(target.resource)).join(", ");
    case "file_list":
      return resourceLabel(action.directory);
    case "file_search":
      return action.query;
    case "code_lookup":
      return action.query;
    case "file_change":
      return action.changes.map((change) => resourceLabel(change.path)).join(", ");
    case "shell":
      return canonicalText(action.command);
    case "code_execute":
      return canonicalText(action.code);
    case "process":
      return action.input ? canonicalText(action.input) : action.process_id;
    case "orchestration":
      return action.executed_children.join(", ") || action.runtime || "";
    case "web_search":
      return action.query ?? action.allowed_domains.join(", ");
    case "web_navigate":
      return action.url ? resourceLabel(action.url) : (action.pattern ?? "");
    case "web_fetch":
      return resourceLabel(action.url);
    case "delegate":
      return canonicalText(action.objective);
    case "task":
      return action.subject ?? (action.description ? canonicalText(action.description) : action.task_id ?? "");
    case "plan":
      return action.title ?? (action.summary ? canonicalText(action.summary) : "");
    case "collaborate":
      return action.summary ?? (action.body ? canonicalText(action.body) : action.recipient ?? action.team ?? "");
    case "question":
      return action.questions[0] ? canonicalText(action.questions[0].prompt) : "";
    case "skill":
      return action.arguments ? canonicalText(action.arguments) : action.name;
    case "workflow":
      return action.objective
        ? canonicalText(action.objective)
        : action.arguments
          ? canonicalText(action.arguments)
          : action.name ?? (action.path ? resourceLabel(action.path) : "");
    case "schedule":
      return action.objective
        ? canonicalText(action.objective)
        : action.reason ?? action.schedule_id ?? "";
    case "artifact":
      return action.name
        ?? (action.description ? canonicalText(action.description) : action.resources[0] ? resourceLabel(action.resources[0]) : "");
    case "tool_discovery":
      return action.query;
    case "mode":
      return action.workspace ? resourceLabel(action.workspace) : action.mode;
    case "finding":
      return action.findings[0]?.title ?? "";
    case "notification":
      return canonicalText(action.summary);
    case "structured_output":
      return action.document.type === "member"
        ? action.document.title ?? action.document.preview
        : action.document.title ?? (action.document.summary ? canonicalText(action.document.summary) : "");
    case "external":
      return action.subject
        ?? (action.resources[0] ? resourceLabel(action.resources[0]) : action.operation);
    case "unknown":
      return "";
  }
}

function actionOutcome(action: ToolAction): string {
  switch (action.type) {
    case "file_read":
      return action.outcome?.truncated ? "truncated" : "";
    case "file_list":
      return action.outcome ? `${action.outcome.entry_count} entries` : "";
    case "file_search":
      return action.outcome ? `${action.outcome.match_count} matches` : "";
    case "code_lookup":
      return action.outcome ? `${action.outcome.total_count} locations` : "";
    case "file_change": {
      const added = action.changes.reduce((sum, change) => sum + (change.lines_added ?? 0), 0);
      const removed = action.changes.reduce((sum, change) => sum + (change.lines_removed ?? 0), 0);
      return added || removed ? `+${added} −${removed}` : "";
    }
    case "shell":
    case "code_execute":
      return action.outcome?.exit_code == null ? "" : `exit ${action.outcome.exit_code}`;
    case "process":
      return action.outcome?.state ?? "";
    case "orchestration":
      return action.executed_child_count == null ? "" : `${action.executed_child_count} children`;
    case "web_search":
      return action.outcome ? `${action.outcome.total_count} sources` : "";
    case "web_navigate":
    case "web_fetch":
      return action.outcome?.http_status == null ? "" : `HTTP ${action.outcome.http_status}`;
    case "delegate":
      return action.child_session_id ? "delegated" : "";
    case "task":
      return action.status ?? "";
    case "plan":
      return action.step_count == null ? "" : `${action.step_count} steps`;
    case "collaborate":
      return action.outcome ? `${action.outcome.total_count} participants` : "";
    case "question":
      return action.answers.length ? `${action.answers.length} answered` : "";
    case "artifact":
      return action.resources.length ? `${action.resources.length} files` : "";
    case "tool_discovery":
      return `${action.total_count} tools`;
    case "finding":
      return `${action.total_count} findings`;
    case "skill":
    case "workflow":
    case "schedule":
    case "mode":
    case "notification":
    case "structured_output":
    case "external":
    case "unknown":
      return "";
  }
}

function callResult(row: TimelineEntry): { result: string; failed: boolean } {
  if (row.body.type !== "tool_use") return { result: "", failed: false };
  const tool = row.body.tool;
  const failed = ["failed", "denied", "cancelled", "timed_out", "interrupted"].includes(
    tool.status.value,
  );
  const failure = tool.failure?.summary ?? tool.failure?.diagnostic;
  if (failure) return { result: firstLine(failure, 44), failed };
  const outcome = actionOutcome(tool.action);
  if (outcome) return { result: firstLine(outcome, 44), failed };
  return {
    result: tool.status.value === "succeeded" ? "" : tool.status.value.replaceAll("_", " "),
    failed,
  };
}

export function toWorkCall(row: TimelineEntry): WorkCall {
  if (row.body.type !== "tool_use") {
    throw new Error(`timeline entry ${row.timeline_id} is not a tool use`);
  }
  const tool = row.body.tool;
  const { result, failed } = callResult(row);
  return {
    row,
    verb:
      tool.action.type === "unknown"
        ? tool.native.name.toLowerCase()
        : tool.action.type.replaceAll("_", " "),
    target: firstLine(actionTarget(tool.action)),
    result,
    failed,
  };
}

/**
 * Fold an ordered timeline into a document: prose rows stand alone, and each
 * contiguous run of tool_use rows becomes one work block (H-1 / M1 at
 * transcript scale). Thinking, system, and summary rows remain readable but
 * render as quieter asides rather than masquerading as user/agent prose.
 */
export function documentBlocks(rows: TimelineEntry[]): DocBlock[] {
  const out: DocBlock[] = [];
  let run: WorkCall[] = [];
  const flush = () => {
    if (run.length === 0) return;
    const stamps = run.map((c) => c.row.at).filter((t): t is number => t !== null);
    const durMs = stamps.length >= 2 ? Math.max(...stamps) - Math.min(...stamps) : null;
    out.push({ type: "work", key: run[0].row.timeline_id, calls: run, durMs });
    run = [];
  };
  for (const row of rows) {
    if (row.body.type === "tool_use") {
      run.push(toWorkCall(row));
      continue;
    }
    flush();
    if (row.body.type === "user" || row.body.type === "agent") {
      if (
        timelineText(row)?.trim() ||
        timelineAttachments(row).length > 0 ||
        timelineCitations(row).length > 0
      ) {
        out.push({ type: "prose", row });
      }
    } else if (timelineText(row)?.trim()) {
      out.push({ type: "aside", row });
    }
  }
  flush();
  return out;
}

export function verbSummary(calls: WorkCall[]): { parts: string[]; failed: number } {
  const order: string[] = [];
  const counts = new Map<string, number>();
  for (const c of calls) {
    if (!counts.has(c.verb)) order.push(c.verb);
    counts.set(c.verb, (counts.get(c.verb) ?? 0) + 1);
  }
  return {
    parts: order.map((v) => `${v} ${counts.get(v)}`),
    failed: calls.filter((c) => c.failed).length,
  };
}

/* ── search snippets: FTS5 marks matches with [brackets] ─────────────────── */

export interface SnippetPart {
  text: string;
  mark: boolean;
}

export function parseSnippet(snippet: string): SnippetPart[] {
  const parts: SnippetPart[] = [];
  const re = /\[([^\]]*)\]/g;
  let at = 0;
  for (let m = re.exec(snippet); m !== null; m = re.exec(snippet)) {
    if (m.index > at) parts.push({ text: snippet.slice(at, m.index), mark: false });
    parts.push({ text: m[1], mark: true });
    at = m.index + m[0].length;
  }
  if (at < snippet.length) parts.push({ text: snippet.slice(at), mark: false });
  return parts;
}
