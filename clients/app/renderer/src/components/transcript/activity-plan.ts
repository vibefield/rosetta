// §04B — group the rendered chronology, after native ownership/deduplication.
// Machine turn boundaries and response metadata are not conversation boundaries.
import type { TimelineEntry } from "@rosetta/protocol";
import type { LedgerLine } from "../../lib/transcript/ledger.ts";
import { runSummary } from "../../lib/transcript/ledger.ts";
import type { Prompt, ResponseHead, Segment } from "../../lib/transcript/turns.ts";
import { collaborationStamp } from "../../lib/transcript/collaboration.ts";
import { noticeText } from "../../lib/transcript/vocabulary.ts";
import type { PlannedSitting } from "./document-plan.ts";

export type ActivityItem =
  | { readonly kind: "tool"; readonly key: string; readonly line: LedgerLine }
  | { readonly kind: "prompt"; readonly key: string; readonly prompt: Prompt }
  | Exclude<Segment, { kind: "ledger" | "prose" }>;

export type ReadingBlock =
  | { readonly kind: "prompt"; readonly key: string; readonly prompts: readonly Prompt[]; readonly at: number | null }
  | { readonly kind: "reply"; readonly key: string; readonly segment: Extract<Segment, { kind: "prose" }>; readonly head: ResponseHead | null; readonly at: number | null }
  | { readonly kind: "activity"; readonly key: string; readonly items: readonly ActivityItem[]; readonly rows: readonly TimelineEntry[]; readonly partial: boolean; readonly at: number | null }
  | { readonly kind: "request"; readonly key: string; readonly item: ActivityItem; readonly at: number | null }
  | { readonly kind: "gap"; readonly key: string; readonly count: number; readonly at: number | null }
  | { readonly kind: "watermark"; readonly key: string; readonly at: number | null };

export interface ReadingSitting {
  readonly sitting: PlannedSitting["sitting"];
  readonly blocks: readonly ReadingBlock[];
}

export function activityRows(item: ActivityItem): readonly TimelineEntry[] {
  if (item.kind === "tool") return item.line.rows;
  if (item.kind === "prompt") return item.prompt.rows;
  return item.kind === "compaction" ? item.rows : [item.row];
}

export function activityRowKey(row: TimelineEntry): string {
  return `${row.session_id}:${row.timeline_id}`;
}

function eventAt(row: TimelineEntry): number | null {
  return row.body.type === "collaboration" ? collaborationStamp(row).at : row.at;
}

/** Tool lines must already have been cut at these boundaries before coalescing. */
export function activityCuts(rows: readonly TimelineEntry[], watermark: number | null, displayedRows: readonly TimelineEntry[] = rows): {
  gaps: Map<string, number>; watermarkId: string | null; cutBefore: Set<string>;
} {
  const gaps = new Map<string, number>();
  const previous = new Map<string, number>();
  let watermarkId: string | null = null;
  for (const row of rows) {
    const ordinal = previous.get(row.session_id);
    if (ordinal !== undefined && row.ordinal > ordinal + 1) gaps.set(row.timeline_id, row.ordinal - ordinal - 1);
    previous.set(row.session_id, row.ordinal);
    if (watermarkId === null && watermark !== null && row.ordinal > watermark) watermarkId = row.timeline_id;
  }
  // A delivery copy may be removed by ownership reconciliation. Keep its
  // paging boundary before the next surviving source event, never discard it.
  if (displayedRows !== rows) {
    const displayed = new Set(displayedRows.map(row => row.timeline_id));
    let next: string | null = null;
    for (let i = rows.length - 1; i >= 0; i -= 1) {
      const id = rows[i].timeline_id;
      if (displayed.has(id)) { next = id; continue; }
      const gap = gaps.get(id);
      if (gap !== undefined) {
        gaps.delete(id);
        if (next !== null) gaps.set(next, (gaps.get(next) ?? 0) + gap);
      }
      if (watermarkId === id) watermarkId = next;
    }
  }
  return { gaps, watermarkId, cutBefore: new Set([...gaps.keys(), ...(watermarkId === null ? [] : [watermarkId])]) };
}

export function activityPlan(plan: readonly PlannedSitting[], options: {
  readonly gaps: ReadonlyMap<string, number>;
  readonly watermarkId: string | null;
  readonly partial: boolean;
}): ReadingSitting[] {
  const headed = new Set<string>();
  let firstContent = true;
  const bounded = new Set<string>();
  return plan.map(({ sitting, parts }) => {
    const blocks: ReadingBlock[] = [];
    let items: ActivityItem[] = [];
    let partial = false;
    const flush = () => {
      if (items.length === 0) return;
      const rows = items.flatMap(activityRows);
      blocks.push({ kind: "activity", key: `activity:${activityRowKey(rows[0])}`, items, rows, partial, at: eventAt(rows[0]) });
      items = [];
      partial = false;
    };
    const boundary = (rows: readonly TimelineEntry[]) => {
      for (const row of rows) {
        if (bounded.has(row.timeline_id)) continue;
        bounded.add(row.timeline_id);
        const gap = options.gaps.get(row.timeline_id);
        if (gap !== undefined) {
          flush();
          blocks.push({ kind: "gap", key: `gap:${activityRowKey(row)}`, count: gap, at: eventAt(row) });
        }
        if (row.timeline_id === options.watermarkId) {
          flush();
          blocks.push({ kind: "watermark", key: `watermark:${activityRowKey(row)}`, at: eventAt(row) });
        }
      }
    };
    const secondary = (item: ActivityItem) => {
      const rows = activityRows(item);
      boundary(rows);
      const request = (item.kind === "tool" && item.line.family === "question") ||
        (item.kind === "marker" && item.notice.type === "permission_requested");
      if (request) {
        flush();
        blocks.push({ kind: "request", key: item.key, item, at: eventAt(rows[0]) });
      } else {
        if (firstContent && options.partial) partial = true;
        items.push(item);
      }
      firstContent = false;
    };
    for (const part of parts) {
      const human: Prompt[] = [];
      const flushPrompts = () => {
        if (human.length === 0) return;
        flush();
        blocks.push({ kind: "prompt", key: human[0].key, prompts: human.splice(0), at: part.at });
        firstContent = false;
      };
      for (const prompt of part.prompts) {
        if (prompt.machine !== null || prompt.fromParent || prompt.row.body.type !== "user") {
          flushPrompts();
          secondary({ kind: "prompt", key: prompt.key, prompt });
        } else {
          // A settings cluster can straddle a loaded gap or live watermark.
          // Finish its earlier prompts before drawing the boundary marker.
          if (prompt.rows.some(row => options.gaps.has(row.timeline_id) || row.timeline_id === options.watermarkId)) flushPrompts();
          boundary([prompt.row]);
          human.push(prompt);
          // A setting's normalized value belongs to the user's command pill.
          // Other machine output receives its own context disclosure.
          if (prompt.commandOutputRow !== null && prompt.setting === null) {
            human[human.length - 1] = { ...prompt, commandOutput: null };
            flushPrompts();
            const row = prompt.commandOutputRow;
            if (row.body.type === "notice") secondary({ kind: "marker", key: row.timeline_id,
              row, at: row.at, notice: row.body.notice, text: noticeText(row.body.notice), tone: "plain" });
          }
        }
      }
      flushPrompts();
      for (const segment of [...part.before, ...part.after]) {
        if (segment.kind === "prose") {
          boundary([segment.row]);
          flush();
          blocks.push({ kind: "reply", key: segment.key, segment,
            head: headed.has(part.turn.key) ? null : part.turn.head, at: segment.at });
          headed.add(part.turn.key);
          firstContent = false;
        } else if (segment.kind === "ledger") {
          for (const member of segment.members) secondary(member.kind === "line"
            ? { kind: "tool", key: member.key, line: member.line } : member);
        } else secondary(segment);
      }
    }
    flush();
    return { sitting, blocks };
  });
}

export interface ActivitySummary {
  readonly nouns: readonly { readonly key: string; readonly text: string; readonly names: readonly string[]; readonly more: number }[];
  readonly failed: number;
  readonly interrupted: number;
  readonly cancelled: number;
  readonly runningRows: readonly TimelineEntry[];
  readonly firstAt: number | null;
  readonly lastAt: number | null;
}

export function summarizeActivity(items: readonly ActivityItem[]): ActivitySummary {
  const lines = items.flatMap(item => item.kind === "tool" ? [item.line] : []);
  const toolSummary = runSummary(lines);
  const counts = new Map<string, number>();
  let failed = toolSummary.failed;
  let interrupted = 0;
  let cancelled = 0;
  const runningRows: TimelineEntry[] = [];
  const count = (name: string) => counts.set(name, (counts.get(name) ?? 0) + 1);
  for (const item of items) {
    const row = activityRows(item)[0];
    if (item.kind === "tool") {
      if (item.line.state === "running") runningRows.push(...item.line.rows);
      interrupted += item.line.rows.filter(row => row.body.type === "tool_use" && row.body.tool.status.value === "interrupted").length;
      cancelled += item.line.rows.filter(row => row.body.type === "tool_use" && row.body.tool.status.value === "cancelled").length;
      continue;
    }
    if (row.body.type === "collaboration") {
      const event = row.body.event;
      count(event.kind === "report" ? "report" : event.kind === "assignment" ? "assignment" :
        event.kind !== "message" ? "team update" : event.channel === "peer" ? "peer message" :
        event.channel === "coordinator" ? "coordinator follow-up" : "team message");
      if (event.status === "failed") failed += 1;
    } else if (item.kind === "thinking") count("thought");
    else if (item.kind === "prompt" && item.prompt.fromParent) count("assignment");
    else count("context event");
    if (item.kind === "marker") {
      if (item.tone === "failed") failed += 1;
      if (item.notice.type === "async_task") {
        if (["running", "pending"].includes(item.notice.status)) runningRows.push(item.row);
        if (item.notice.status === "interrupted") interrupted += 1;
        if (item.notice.status === "cancelled") cancelled += 1;
      }
    }
  }
  const nouns: ActivitySummary["nouns"][number][] = toolSummary.nouns.map(noun => ({ ...noun, key: noun.family }));
  for (const [name, total] of counts) nouns.push({ key: name, text: `${total} ${name}${total === 1 ? "" : "s"}`, names: [], more: 0 });
  const stamps = items.flatMap(activityRows).map(eventAt).filter((at): at is number => at !== null);
  return { nouns, failed, interrupted, cancelled, runningRows, firstAt: stamps[0] ?? null, lastAt: stamps.at(-1) ?? null };
}
