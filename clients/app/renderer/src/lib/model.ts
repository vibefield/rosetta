// View models: rosetta wire rows → what the room draws. Pure functions of
// their inputs; every derived figure states its window at the callsite.
import type {
  ContributionRow,
  HeatmapDay,
  MessageRow,
  ProjectRow,
  SessionRow,
} from "@rosetta/protocol";
import type { AgentShare } from "./agents";
import { projectTint } from "./tint";

export interface ProjectStats {
  /** Daily token totals over the ribbon's window (53 Sunday-aligned weeks
   * ending today), from /heatmap?since_ms=…. */
  heat: HeatmapDay[];
  /** Per-root-session contribution (tokens, messages, …), whole project life. */
  contribution: ContributionRow[];
}

export interface ProjectVM {
  row: ProjectRow;
  tint: string;
  /** Tokens over the last 30 days — the ledger's stated window. */
  tokens30: number;
  /** 15 two-day buckets, oldest first — the activity shape. */
  buckets: number[];
  /** 53×7 daily totals from the grid's Sunday; cells after today are -1.
   * The ribbon is all of it and the 30-day figures are its tail, so the
   * charts are views of one number and cannot disagree. */
  yearCells: number[];
  /** Index of today inside yearCells. */
  todayIndex: number;
  /** Tokens over the whole 53-week window — the meta block's stated window. */
  yearTokens: number;
  /** Message rows across the project's whole life (contribution sum). */
  messagesLife: number;
  agents: AgentShare[];
  lastActiveMs: number | null;
  sessionCount: number;
}

const DAY_MS = 86_400_000;

/* ── the ribbon's grid: 53 columns of weeks, Sunday-aligned, ending today ── */

export const HEAT_WEEKS = 53;
export const HEAT_DAYS = HEAT_WEEKS * 7;

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Midnight of the Sunday opening the grid's first column. */
export function yearGridStartMs(nowMs: number): number {
  const today = startOfDay(nowMs);
  return today - (52 * 7 + new Date(today).getDay()) * DAY_MS;
}

/** GitHub-style streak facts, walked from the series rather than declared
 * beside it. */
export function heatStats(cells: number[], todayIndex: number): {
  active: number;
  current: number;
  longest: number;
} {
  let active = 0;
  let longest = 0;
  let run = 0;
  for (let i = 0; i <= todayIndex; i += 1) {
    if (cells[i] > 0) {
      active += 1;
      run += 1;
      if (run > longest) longest = run;
    } else {
      run = 0;
    }
  }
  let current = 0;
  for (let i = todayIndex; i >= 0 && cells[i] > 0; i -= 1) current += 1;
  return { active, current, longest };
}

/** The viewer's offset, minutes east of UTC — passed to /heatmap so the
 * daemon buckets days the way this screen will label them. */
export function viewerOffsetMinutes(): number {
  return -new Date().getTimezoneOffset();
}

/** The viewer-offset day key for a timestamp — the exact form the heatmap
 * endpoint was asked to bucket in. */
export function dayKey(ms: number): string {
  const d = new Date(ms + viewerOffsetMinutes() * 60_000);
  return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, "0")}-${String(
    d.getUTCDate(),
  ).padStart(2, "0")}`;
}

/** The full 53×7 grid, zero-filled from sparse heatmap days; cells after
 * today hold -1 (not yet, which is not the same as empty). */
function yearSeries(heat: HeatmapDay[], nowMs: number): { cells: number[]; todayIndex: number } {
  const byDay = new Map(heat.map((d) => [d.day, d.tokens]));
  const start = yearGridStartMs(nowMs);
  const today = dayKey(nowMs);
  const cells: number[] = [];
  let todayIndex = HEAT_DAYS - 1;
  for (let i = 0; i < HEAT_DAYS; i += 1) {
    const key = dayKey(start + i * DAY_MS + DAY_MS / 2);
    if (key === today) todayIndex = i;
    cells.push(i > todayIndex ? -1 : (byDay.get(key) ?? 0));
  }
  return { cells, todayIndex };
}

export function buildProjectVM(
  row: ProjectRow,
  sessions: SessionRow[],
  stats: ProjectStats | undefined,
  kindOf: (runtimeId: string) => string,
  nowMs: number,
): ProjectVM {
  const own = sessions.filter((s) => s.project_id === row.id && s.id === s.root_session_id);
  const { cells: yearCells, todayIndex } = yearSeries(stats?.heat ?? [], nowMs);
  // The ledger's 30-day figures are the same series' tail, so the ribbon,
  // the sparkline, and the figure can never disagree.
  const daily: number[] = [];
  for (let i = 29; i >= 0; i -= 1) daily.push(Math.max(0, yearCells[todayIndex - i] ?? 0));
  const buckets: number[] = [];
  for (let i = 0; i < 15; i += 1) buckets.push((daily[i * 2] ?? 0) + (daily[i * 2 + 1] ?? 0));
  const tokens30 = daily.reduce((t, v) => t + v, 0);
  const yearTokens = yearCells.reduce((t, v) => t + Math.max(0, v), 0);
  const messagesLife = (stats?.contribution ?? []).reduce((t, r) => t + r.messages, 0);

  const byKind = new Map<string, number>();
  for (const s of own) {
    const kind = kindOf(s.runtime_id);
    byKind.set(kind, (byKind.get(kind) ?? 0) + 1);
  }
  const total = own.length || 1;
  const agents: AgentShare[] = [...byKind.entries()]
    .map(([kind, count]) => ({ kind, pct: Math.round((count / total) * 100) }))
    .sort((a, b) => b.pct - a.pct);

  const lastActiveMs = own.length ? Math.max(...own.map((s) => s.updated_at)) : null;
  return {
    row,
    tint: projectTint(row.id),
    tokens30,
    buckets,
    yearCells,
    todayIndex,
    yearTokens,
    messagesLife,
    agents,
    lastActiveMs,
    sessionCount: own.length,
  };
}

/* ── the work block: a contiguous run of tool calls is ONE object ────────── */

export interface WorkCall {
  row: MessageRow;
  verb: string;
  target: string;
  result: string;
  failed: boolean;
}

export type DocBlock =
  | { type: "prose"; row: MessageRow }
  | { type: "work"; key: string; calls: WorkCall[]; durMs: number | null };

function firstLine(text: string, max = 90): string {
  const line = text.split("\n", 1)[0].trim();
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

function callTarget(row: MessageRow): string {
  const input = row.tool?.input;
  if (input && typeof input === "object" && !Array.isArray(input)) {
    const record = input as Record<string, unknown>;
    for (const key of ["command", "file_path", "path", "pattern", "query", "url", "prompt", "description"]) {
      const value = record[key];
      if (typeof value === "string" && value.trim() !== "") return firstLine(value);
    }
    for (const value of Object.values(record)) {
      if (typeof value === "string" && value.trim() !== "") return firstLine(value);
    }
  }
  return row.content ? firstLine(row.content) : "";
}

function callResult(row: MessageRow): { result: string; failed: boolean } {
  const failed = row.tool?.is_error === true;
  const text = row.tool?.result;
  if (text && text.trim() !== "") return { result: firstLine(text, 44), failed };
  return { result: failed ? "error" : "", failed };
}

export function toWorkCall(row: MessageRow): WorkCall {
  const { result, failed } = callResult(row);
  return {
    row,
    verb: (row.tool?.name ?? row.subtype ?? "tool").toLowerCase(),
    target: callTarget(row),
    result,
    failed,
  };
}

/**
 * Fold an ordered timeline into a document: prose rows stand alone, and each
 * contiguous run of tool_use rows becomes one work block (H-1 / M1 at
 * transcript scale). Thinking, system, and summary rows are not drawn yet.
 */
export function documentBlocks(rows: MessageRow[]): DocBlock[] {
  const out: DocBlock[] = [];
  let run: WorkCall[] = [];
  const flush = () => {
    if (run.length === 0) return;
    const stamps = run.map((c) => c.row.ts).filter((t): t is number => t !== null);
    const durMs = stamps.length >= 2 ? Math.max(...stamps) - Math.min(...stamps) : null;
    out.push({ type: "work", key: run[0].row.timeline_id, calls: run, durMs });
    run = [];
  };
  for (const row of rows) {
    if (row.kind === "tool_use") {
      run.push(toWorkCall(row));
      continue;
    }
    flush();
    if (row.kind === "user" || row.kind === "agent") {
      if (row.content && row.content.trim() !== "") out.push({ type: "prose", row });
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
