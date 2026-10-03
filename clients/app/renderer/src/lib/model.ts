// View models: rosetta wire rows → what the room draws. Pure functions of
// their inputs; every derived figure states its window at the callsite.
import type {
  ContributionRow,
  HeatmapDay,
  ProjectRow,
  SessionRow,
} from "@rosetta/protocol";
import type { AgentShare } from "./agents";
import { NO_SPEND, addSpend, spendOf, sumSpend } from "./spend";
import type { Spend } from "./spend";
import { projectTint } from "./tint";
import { sessionLastActive } from "./session-activity";

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
  /** Tokens across the project's whole life (root contributions include
   * their subagent trees). */
  readonly lifetimeTokens: number;
  /** List-price spend across the project's whole life — the project card's
   * primary figure and the ledger's default ranking (§4.1.3). */
  readonly lifetimeSpend: Spend;
  /** List-price spend over the last 30 days, the same heatmap tail. */
  readonly spendLast30Days: Spend;
  /** List-price spend over the 53-week window. */
  readonly spendLast53Weeks: Spend;
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
  /** Folded over session stream_state. Aggregate requests have their own
   * statsReady gate and may finish after transcripts become browseable. */
  readonly readiness: "pending" | "partial" | "ready";
  /** Aggregate requests finish separately from transcript publication. */
  readonly statsReady: boolean;
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
    (session) => session.parent_session_id === null,
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
    [dailyTokensLast30Days[bucketIndex * 2], dailyTokensLast30Days[bucketIndex * 2 + 1]].reduce<number>(
      (total, value) => total + (value ?? 0),
      0,
    ),
  );
  const tokensLast30Days = dailyTokensLast30Days.reduce((total, value) => total + value, 0);
  const tokensLast53Weeks = dailyTokens.reduce(
    (total, value) => total + Math.max(0, value),
    0,
  );
  const lifetimeTokens = (projectStats?.contribution ?? []).reduce(
    (total, contribution) => total + contribution.tokens,
    0,
  );
  const lifetimeSpend = sumSpend(projectStats?.contribution ?? []);
  // Spend windows come from the same heatmap days as the token windows.
  const thirtyDayKeys = new Set(
    Array.from({ length: 30 }, (_, dayIndex) =>
      getCalendarDayKey(currentTimestamp - (29 - dayIndex) * DAY_IN_MS),
    ),
  );
  let spendLast30Days = NO_SPEND;
  let spendLast53Weeks = NO_SPEND;
  for (const day of projectStats?.heat ?? []) {
    const spend = spendOf(day);
    spendLast53Weeks = addSpend(spendLast53Weeks, spend);
    if (thirtyDayKeys.has(day.day)) spendLast30Days = addSpend(spendLast30Days, spend);
  }
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

  const lastActiveMs = rootSessions.reduce<number | null>((latest, session) => {
    const at = sessionLastActive(session);
    return at === null ? latest : latest === null ? at : Math.max(latest, at);
  }, null);
  return {
    row: projectRow,
    tint: projectTint(projectRow.id),
    lifetimeTokens,
    lifetimeSpend,
    spendLast30Days,
    spendLast53Weeks,
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
    statsReady: projectStats !== undefined,
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
    parts.push({ text: m[1]!, mark: true });
    at = m.index + m[0].length;
  }
  if (at < snippet.length) parts.push({ text: snippet.slice(at), mark: false });
  return parts;
}
