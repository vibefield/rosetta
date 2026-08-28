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
  /** Daily token totals for the last 30 days, from /heatmap?since_ms=…. */
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
  agents: AgentShare[];
  lastActiveMs: number | null;
  sessionCount: number;
}

const DAY_MS = 86_400_000;

/** The viewer's offset, minutes east of UTC — passed to /heatmap so the
 * daemon buckets days the way this screen will label them. */
export function viewerOffsetMinutes(): number {
  return -new Date().getTimezoneOffset();
}

/** 30 daily totals ending today, zero-filled from sparse heatmap days.
 * Days are keyed in the same viewer offset the heatmap was asked in. */
function dailySeries(heat: HeatmapDay[], nowMs: number): number[] {
  const byDay = new Map(heat.map((d) => [d.day, d.tokens]));
  const shift = viewerOffsetMinutes() * 60_000;
  const out: number[] = [];
  for (let i = 29; i >= 0; i -= 1) {
    const d = new Date(nowMs - i * DAY_MS + shift);
    const key = `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, "0")}-${String(
      d.getUTCDate(),
    ).padStart(2, "0")}`;
    out.push(byDay.get(key) ?? 0);
  }
  return out;
}

export function buildProjectVM(
  row: ProjectRow,
  sessions: SessionRow[],
  stats: ProjectStats | undefined,
  kindOf: (runtimeId: string) => string,
  nowMs: number,
): ProjectVM {
  const own = sessions.filter((s) => s.project_id === row.id && s.id === s.root_session_id);
  const daily = stats ? dailySeries(stats.heat, nowMs) : [];
  const buckets: number[] = [];
  for (let i = 0; i < 15; i += 1) buckets.push((daily[i * 2] ?? 0) + (daily[i * 2 + 1] ?? 0));
  const tokens30 = daily.reduce((t, v) => t + v, 0);

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
  return { row, tint: projectTint(row.id), tokens30, buckets, agents, lastActiveMs, sessionCount: own.length };
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
