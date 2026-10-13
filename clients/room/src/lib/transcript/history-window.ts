// Paging follows visible conversation boundaries. Canonical rows remain in the
// subscription cache; a clipped leading activity prefix waits for the next page.
import type { TimelineEntry, TimelinePage } from "@vibefield/rosetta-protocol";
import { readAttachments } from "./turns";
import { canonicalText } from "./vocabulary";

export function isPrimaryMessage(row: TimelineEntry): boolean {
  if (row.body.type === "user") return row.group.kind !== "parent_invocation";
  if (row.body.type !== "agent") return false;
  const { text, attachments } = readAttachments(
    row.body.text === null ? null : canonicalText(row.body.text), row.body.attachments,
  );
  return text !== null || attachments.length > 0 || row.body.citations.length > 0;
}

/** A primary message is a safe start. If a bounded read contains only activity,
 * show that activity honestly as incomplete rather than hiding the whole page. */
export function conversationStart(rows: readonly TimelineEntry[], hasEarlier: boolean): number | null {
  return (hasEarlier ? rows.find(isPrimaryMessage) ?? rows[0] : rows[0])?.ordinal ?? null;
}

/** Read enough for a useful conversation section, with a hard work bound for
 * enormous tool/team runs. Each result is still held until the reader reveals it. */
export async function readConversationPage(
  fetchPage: (cursor: string, limit: number) => Promise<TimelinePage>,
  cursor: string,
  pageSize = 200,
): Promise<TimelinePage> {
  const rows = new Map<string, TimelineEntry>();
  let next: string | null = cursor;
  let watermark = "";
  for (let pages = 0; pages < 5 && next !== null; pages += 1) {
    const previous: string = next;
    const page = await fetchPage(next, pageSize);
    watermark = page.watermark;
    for (const row of page.entries) rows.set(row.timeline_id, row);
    next = page.cursor;
    if (next === previous) throw new Error("History cursor did not advance");
    if ([...rows.values()].filter(isPrimaryMessage).length >= 6) break;
  }
  return { entries: [...rows.values()].sort((a, b) => a.ordinal - b.ordinal), cursor: next, watermark };
}

/** A shared event may enter only a covered gap in the parent chronology. An
 * exact search anchor does not claim coverage of the missing rows beside it.
 * Undated events wait for complete history because no partial window can place
 * them reliably. Native parent order and delivery-id ownership remain intact. */
export function withinHistoryWindow(
  at: number | null,
  parents: readonly TimelineEntry[],
  hasEarlier: boolean,
): boolean {
  if (parents.length === 0) return !hasEarlier;
  const hasGaps = parents.some((row, i) => i > 0 && row.ordinal > parents[i - 1]!.ordinal + 1);
  if (at === null) return !hasEarlier && !hasGaps;
  const firstAt = parents.find(row => row.at !== null)?.at;
  if (hasEarlier && (firstAt == null || at < firstAt)) return false;
  const before = parents.findIndex(row => row.at !== null && row.at > at);
  return before <= 0 || parents[before]!.ordinal === parents[before - 1]!.ordinal + 1 ||
    at === parents[before - 1]!.at;
}
