// A host's sections over one of the room's lists (R3): which rows each
// section draws, in whose order. The room's own order comes in; the host's
// keys come out as runs the list draws under their heads.
import type { RoomSection } from "../host-inputs";

export interface ArrangedSection<T> {
  readonly section: RoomSection;
  /** Its place among the sections given, which the list's motion keys on. */
  readonly index: number;
  /** Its rows, in the order they are drawn. */
  readonly rows: readonly T[];
  /** Whose order they keep: `room` draws a session list's day heads. */
  readonly order: "host" | "room";
}

/**
 * The rows each section draws. A row two sections name is drawn in the
 * first; a row no section names goes to the first section without `ids`, or
 * nowhere when every section names its own — so a host filter hides rows by
 * naming fewer. Ids the room does not hold are skipped.
 */
export function arrangeSections<T>(
  sections: readonly RoomSection[],
  rowsInRoomOrder: readonly T[],
  idOf: (row: T) => string,
): ArrangedSection<T>[] {
  const byId = new Map(rowsInRoomOrder.map((row) => [idOf(row), row]));
  const rank = new Map(rowsInRoomOrder.map((row, at) => [idOf(row), at]));
  const named = new Set(sections.flatMap((section) => section.ids ?? []));
  const drawn = new Set<string>();
  let restTaken = false;
  return sections.map((section, index) => {
    let rows: T[];
    let order: "host" | "room";
    if (section.ids === undefined) {
      order = "room";
      rows = restTaken ? [] : rowsInRoomOrder.filter((row) => !named.has(idOf(row)));
      restTaken = true;
    } else {
      order = section.order ?? "host";
      const ids = [...new Set(section.ids)].filter((id) => byId.has(id) && !drawn.has(id));
      if (order === "room") ids.sort((left, right) => rank.get(left)! - rank.get(right)!);
      rows = ids.map((id) => byId.get(id)!);
    }
    for (const row of rows) drawn.add(idOf(row));
    return { section, index, rows, order };
  });
}

/** How many rows a folded section says it holds: the host's figure, or its rows. */
export function foldCount<T>(arranged: ArrangedSection<T>): number {
  return arranged.section.fold?.count ?? arranged.rows.length;
}
