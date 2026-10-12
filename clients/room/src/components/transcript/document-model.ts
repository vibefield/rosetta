// One derivation of the document, shared by the column and by every child run.
//
// The turns, the sittings, the day list and the plan that reconciles them are
// all pure functions of the loaded rows; this hook memoises them together so
// the column and the document can never be looking at two different documents.
//
// It also answers two questions the DOM asks:
//
//   · Which element DRAWS a given row? A fold collapses several rows into one
//     line, so a search hit's `timeline_id` may have no element of its own.
//     The representative map sends the landing to the element that contains it
//     instead of failing to find one.
//   · Where did the projection skip? Ordinals are contiguous in a loaded page,
//     so a jump is a real absence — D33's data gap, stated rather than
//     silently closed over.
import { useMemo } from "react";
import type { SessionRow, TimelineEntry } from "@vibefield/rosetta-protocol";
import type { DayEntry, DocumentOptions, Sitting, Turn } from "../../lib/transcript";
import { dayList, documentTurns, sittings } from "../../lib/transcript";
import type { PlannedSitting } from "./document-plan";
import { documentPlan } from "./document-plan";

export interface TranscriptModel {
  readonly turns: readonly Turn[];
  readonly sittings: readonly Sitting[];
  readonly days: readonly DayEntry[];
  readonly plan: readonly PlannedSitting[];
  /** `timeline_id` → the `timeline_id` whose element actually draws it. */
  readonly representatives: ReadonlyMap<string, string>;
  /** The row after a discontinuity → how many rows the page is missing. */
  readonly gaps: ReadonlyMap<string, number>;
}

function buildRepresentatives(turns: readonly Turn[]): Map<string, string> {
  const map = new Map<string, string>();
  const claim = (rows: readonly TimelineEntry[], by: string) => {
    for (const row of rows) map.set(row.timeline_id, by);
  };
  for (const turn of turns) {
    if (turn.prompt !== null) {
      claim(turn.prompt.rows, turn.prompt.row.timeline_id);
      if (turn.prompt.commandOutputRow !== null && turn.prompt.setting === null) {
        const id = turn.prompt.commandOutputRow.timeline_id;
        map.set(id, id);
      }
    }
    for (const segment of turn.segments) {
      if (segment.kind === "compaction") {
        claim(segment.rows, segment.row.timeline_id);
        continue;
      }
      if (segment.kind === "ledger") {
        for (const member of segment.members) {
          if (member.kind !== "line") {
            map.set(member.row.timeline_id, member.row.timeline_id);
            continue;
          }
          const key = member.line.rows[0]?.timeline_id;
          if (key !== undefined) claim(member.line.rows, key);
        }
        continue;
      }
      map.set(segment.row.timeline_id, segment.row.timeline_id);
    }
  }
  return map;
}

function buildGaps(rows: readonly TimelineEntry[]): Map<string, number> {
  const gaps = new Map<string, number>();
  const previousBySession = new Map<string, number>();
  for (const row of rows) {
    const previous = previousBySession.get(row.session_id);
    const missing = previous === undefined ? 0 : row.ordinal - previous - 1;
    if (missing > 0) gaps.set(row.timeline_id, missing);
    previousBySession.set(row.session_id, row.ordinal);
  }
  return gaps;
}

export function transcriptModel(
  rows: readonly TimelineEntry[],
  session: SessionRow | null,
  homeDirectory: string | null,
  now: number,
  options: DocumentOptions = {},
): TranscriptModel {
  // Sittings first: a run never straddles a pill, so the turns are cut at
  // every sitting's first row (`cutBefore`) before they are folded.
  const list = sittings(rows, now);
  const gaps = buildGaps(rows);
  const cutBefore = new Set(
    [...(options.cutBefore ?? gaps.keys()), ...list
      .slice(1)
      .map((sitting) => sitting.rows[0]?.timeline_id)
      .filter((id): id is string => id !== undefined)],
  );
  const turns = documentTurns(rows, {
    ...options,
    cutBefore,
    context: { cwd: session?.cwd ?? null, home: homeDirectory },
  });
  return {
    turns,
    sittings: list,
    days: dayList(list),
    plan: documentPlan(turns, list),
    representatives: buildRepresentatives(turns),
    gaps,
  };
}

export function useTranscriptModel(
  rows: readonly TimelineEntry[],
  session: SessionRow | null,
  homeDirectory: string | null,
  now: number,
  options: DocumentOptions = {},
): TranscriptModel {
  const cwd = session?.cwd ?? null;
  const sharedKey = [...(options.sharedCollaborationSessionIds ?? [])].sort().join("\u0000");
  const cutKey = [...(options.cutBefore ?? [])].sort().join("\u0000");
  return useMemo(() => {
    const sharedCollaborationSessionIds = new Set(
      sharedKey === "" ? [] : sharedKey.split("\u0000"),
    );
    return transcriptModel(rows, session, homeDirectory, now, {
      ...options,
      sharedCollaborationSessionIds,
    });
    // `now` only names the day a stamp falls in; re-deriving the whole
    // document on every clock tick would be a memo that never hits.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows, cwd, homeDirectory, sharedKey, cutKey]);
}
