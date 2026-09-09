// The document's layout plan — where the turns sit inside the sittings.
//
// Two frozen models describe the same rows from two angles: `documentTurns`
// cuts them into prompt-and-answer, `sittings` cuts them into day and pause.
// The two cuts do NOT nest — a fifteen-minute silence can fall in the middle
// of a reply (§07's own measured example: Grok resumed after ten and a half
// hours with no prompt between). This module reconciles them.
//
// Two decisions the record left to the implementation:
//
//  1. A sitting boundary inside a turn SPLITS the turn. Every sitting must be
//     its own `section` or the sticky pills stack on each other instead of
//     pushing each other out (§07), and a section cannot start in the middle
//     of an element. So the turn's segments are cut at the boundary and the
//     second part continues without a head — T-4 says one head per response,
//     and a continuation is the same response.
//
//  2. Only adjacent successful setting commands may cluster. Human prose and
//     parent-agent prompts always remain standalone messages with their own
//     anchor and actions. A response, marker, or sitting boundary also ends a
//     configuration group.
//
// Pure: no clock, no DOM, no React.
import type { TimelineEntry } from "@rosetta/protocol";
import type { Prompt, Segment, Sitting, Turn } from "../../lib/transcript";

/** One drawn block: a slice of one turn, inside exactly one sitting. */
export interface TurnPart {
  readonly key: string;
  readonly sittingIndex: number;
  readonly turn: Turn;
  /** The prompts this part opens with — several only for grouped settings. */
  readonly prompts: readonly Prompt[];
  /** Segments before the head (leading markers) and after it. */
  readonly before: readonly Segment[];
  readonly after: readonly Segment[];
  /** T-4's eyebrow belongs to this part. */
  readonly showHead: boolean;
  /** The first row drawn here — the paging hook's landing contract. */
  readonly firstRowId: string | null;
  /** The first stamp under this part, for placing an unanchored child run. */
  readonly at: number | null;
}

export interface PlannedSitting {
  readonly sitting: Sitting;
  readonly parts: readonly TurnPart[];
}

function firstStampOf(prompts: readonly Prompt[], segments: readonly Segment[]): number | null {
  for (const prompt of prompts) if (prompt.at !== null) return prompt.at;
  for (const segment of segments) {
    for (const row of segmentRows(segment)) if (row.at !== null) return row.at;
  }
  return null;
}

function firstRowIdOf(prompts: readonly Prompt[], segments: readonly Segment[]): string | null {
  const prompt = prompts[0];
  if (prompt !== undefined) return prompt.row.timeline_id;
  const segment = segments[0];
  if (segment === undefined) return null;
  return segment.kind === "ledger" ? (segment.rows[0]?.timeline_id ?? null) : segment.row.timeline_id;
}

function segmentRows(segment: Segment): readonly TimelineEntry[] {
  return segment.kind === "ledger" || segment.kind === "compaction" ? segment.rows : [segment.row];
}

/** A turn's own first row, for the sitting it belongs to. */
function turnSitting(turn: Turn, indexOf: (row: TimelineEntry) => number): number {
  if (turn.prompt !== null) return indexOf(turn.prompt.row);
  const first = turn.segments[0];
  return first === undefined ? 0 : indexOf(segmentRows(first)[0]);
}

/** Adjacent setting turns that share one compact configuration group. */
function clusterGroups(turns: readonly Turn[], sittingOf: (turn: Turn) => number): Turn[][] {
  const groups: Turn[][] = [];
  for (const turn of turns) {
    const current = groups.at(-1);
    const previous = current?.at(-1);
    const joins =
      current !== undefined &&
      previous !== undefined &&
      turn.clusteredWithPrevious &&
      turn.prompt !== null &&
      previous.prompt !== null &&
      turn.prompt.setting !== null &&
      previous.prompt.setting !== null &&
      !turn.prompt.fromParent &&
      !previous.prompt.fromParent &&
      previous.segments.length === 0 &&
      sittingOf(previous) === sittingOf(turn);
    if (joins) current.push(turn);
    else groups.push([turn]);
  }
  return groups;
}

/**
 * Fold turns and sittings into the sections the column draws. Sittings that
 * end up with nothing drawable are dropped: a pill over an empty section would
 * be a cut with nothing on the far side of it.
 */
export function documentPlan(
  turns: readonly Turn[],
  sittings: readonly Sitting[],
): PlannedSitting[] {
  const indexByRow = new Map<string, number>();
  sittings.forEach((sitting, index) => {
    for (const row of sitting.rows) indexByRow.set(row.timeline_id, index);
  });
  const indexOf = (row: TimelineEntry) => indexByRow.get(row.timeline_id) ?? 0;
  const sittingOf = (turn: Turn) => turnSitting(turn, indexOf);

  const parts: TurnPart[] = [];
  for (const group of clusterGroups(turns, sittingOf)) {
    // Only the last turn of a cluster can carry an answer, by construction.
    const turn = group[group.length - 1];
    const prompts = group
      .map((member) => member.prompt)
      .filter((prompt): prompt is Prompt => prompt !== null);
    const openIndex = prompts[0] === undefined ? null : indexOf(prompts[0].row);

    // Cut the segments wherever the sitting changes under them.
    const slices: { index: number; segments: Segment[]; from: number }[] = [];
    turn.segments.forEach((segment, at) => {
      const index = indexOf(segmentRows(segment)[0]);
      const last = slices.at(-1);
      if (last !== undefined && last.index === index) last.segments.push(segment);
      else slices.push({ index, segments: [segment], from: at });
    });

    const headSlice = turn.head === null ? -1 : slices.findIndex(
      (slice) => turn.headIndex >= slice.from && turn.headIndex < slice.from + slice.segments.length,
    );
    // A head with no segments to sit above still belongs to the reply that
    // reported it; it rides with the prompt rather than disappearing.
    const headOnOpener = turn.head !== null && headSlice === -1;

    if (slices.length === 0) {
      parts.push({
        key: turn.key,
        sittingIndex: openIndex ?? 0,
        turn,
        prompts,
        before: [],
        after: [],
        showHead: turn.head !== null,
        firstRowId: firstRowIdOf(prompts, []),
        at: firstStampOf(prompts, []),
      });
      continue;
    }

    slices.forEach((slice, at) => {
      const opensWithPrompt = at === 0 && openIndex !== null && openIndex === slice.index;
      const slicePrompts = at === 0 && (opensWithPrompt || openIndex === null) ? prompts : [];
      const cut = Math.max(0, Math.min(turn.headIndex - slice.from, slice.segments.length));
      const showHead = at === headSlice || (headOnOpener && at === 0);
      parts.push({
        key: `${turn.key}:${at}`,
        sittingIndex: slice.index,
        turn,
        prompts: slicePrompts,
        before: slice.segments.slice(0, cut),
        after: slice.segments.slice(cut),
        showHead,
        firstRowId: firstRowIdOf(slicePrompts, slice.segments),
        at: firstStampOf(slicePrompts, slice.segments),
      });
    });

    // The prompt fell in an earlier sitting than every segment: it needs its
    // own part so its pill and its panel are not filed under the reply's day.
    if (openIndex !== null && slices[0].index !== openIndex) {
      parts.splice(parts.length - slices.length, 0, {
        key: `${turn.key}:opener`,
        sittingIndex: openIndex,
        turn,
        prompts,
        before: [],
        after: [],
        showHead: false,
        firstRowId: firstRowIdOf(prompts, []),
        at: firstStampOf(prompts, []),
      });
    }
  }

  const planned: PlannedSitting[] = sittings.map((sitting) => ({ sitting, parts: [] as TurnPart[] }));
  for (const part of parts) {
    const target = planned[part.sittingIndex] ?? planned[0];
    if (target === undefined) continue;
    (target.parts as TurnPart[]).push(part);
  }
  return planned.filter((entry) => entry.parts.length > 0);
}
