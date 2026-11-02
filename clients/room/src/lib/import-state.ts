// How the room speaks about sessions the import has not reached.
//
// A cold import reads the newest conversation trees first, and each tree
// opens as soon as it closes. While it runs, the session list keeps a
// "Not imported yet" group at its foot, the transcript says why a session is
// still empty, and a held list counts the conversations that arrived. Every
// fact here comes from fields the daemon already sends: `stream_state`, the
// session's own metadata, and the parent links between sessions.
import type { Ingestion, SessionRow, StreamState } from "@vibefield/rosetta-protocol";
import type { ListPosition } from "./use-deferred-list-order";

/** Where a session stands with the import, for the words the room uses.
 * - `not-imported`: the import is running and has not reached it yet.
 * - `unavailable`: no import will reach it, because its own transcript was
 *   never found (see `isMetadataOnly`).
 * - `null`: nothing to say; the session reads as it always did. */
export type ImportStanding = "not-imported" | "unavailable" | null;

/** The list group's header, in place of a date. */
export const NOT_IMPORTED_LABEL = "Not imported yet";

/** A row the metadata pass made only because a descendant names it as its
 * parent or root. Its own transcript was not found, so the row is Pending
 * with no cwd, title, summary or delegation, and no import will ever reach
 * it. Children are what tell it apart from a real session whose head said
 * nothing: every such row has a direct child, since a root made for a
 * grandchild comes with the missing parent between them, which names it. */
export function isMetadataOnly(session: SessionRow, hasChildren: boolean): boolean {
  return hasChildren && session.stream_state === "pending" && session.cwd === null &&
    session.title === null && session.summary === null && session.delegation === null;
}

export function importStanding(
  session: SessionRow, duringBackfill: boolean, hasChildren: boolean,
): ImportStanding {
  if (isMetadataOnly(session, hasChildren)) return "unavailable";
  return duringBackfill && session.stream_state === "pending" ? "not-imported" : null;
}

/** When the import opens a session it has not reached yet.
 * - `newest-first`: a fresh build opens each conversation tree as it closes.
 * - `resumed`: a resumed build (`resumedRun`) withholds every session it has
 *   not opened before until it publishes, so they open when it finishes.
 * - `stopped`: a paused or failed run reaches nothing until it continues. */
export type ImportCourse = "newest-first" | "resumed" | "stopped";

export function importCourse(run: Ingestion | null | undefined, resumed: boolean): ImportCourse {
  if (run?.state === "paused" || run?.state === "pausing" || run?.state === "failed") return "stopped";
  return resumed ? "resumed" : "newest-first";
}

/** The ids some session names as its parent: the sessions with children. */
export function parentIds(sessions: readonly SessionRow[]): ReadonlySet<string> {
  const ids = new Set<string>();
  for (const session of sessions) {
    if (session.parent_session_id !== null) ids.add(session.parent_session_id);
  }
  return ids;
}

/** The list's draw order during an import: every other row where the room's
 * order put it, then the not-imported rows, ordered by `compare`. */
export function notImportedLast<T>(
  rows: readonly T[], notImported: (row: T) => boolean, compare: (left: T, right: T) => number,
): T[] {
  const waiting = rows.filter(notImported);
  if (waiting.length === 0) return [...rows];
  return [...rows.filter((row) => !notImported(row)), ...waiting.sort(compare)];
}

/* ── a held list's arrivals ───────────────────────────────────────────── */

/** Conversations a held list still draws in the not-imported group while
 * the wanted order draws them as imported. */
export interface ImportArrivals {
  readonly count: number;
  /** Every arrival lands below the row the reader is looking at. */
  readonly older: boolean;
}

/**
 * What arrived while the reader held the list.
 *
 * `anchorIds` are the rows at the top of the reader's view, nearest first; an
 * empty list means the view is at the top, so its first row is the anchor.
 * Null when no held not-imported row was imported.
 */
export function importArrivals(
  held: readonly ListPosition[], wanted: readonly ListPosition[], anchorIds: readonly string[],
): ImportArrivals | null {
  const waiting = new Set(held.flatMap((item) => (item.pending === true ? [item.id] : [])));
  if (waiting.size === 0) return null;
  const rank = new Map(wanted.map((item, at) => [item.id, at]));
  const landed = wanted.flatMap((item, at) => (waiting.has(item.id) && item.pending !== true ? [at] : []));
  if (landed.length === 0) return null;
  const anchor = [...anchorIds, ...(held[0] === undefined ? [] : [held[0].id])]
    .map((id) => rank.get(id))
    .find((at) => at !== undefined);
  return { count: landed.length, older: anchor !== undefined && landed.every((at) => at > anchor) };
}

/** "3 older conversations imported", or without "older" when one lands above. */
export function arrivalsLabel(arrivals: ImportArrivals): string {
  const noun = arrivals.count === 1 ? "conversation" : "conversations";
  return `${arrivals.count.toLocaleString()} ${arrivals.older ? "older " : ""}${noun} imported`;
}

/* ── the transcript's flash guard ─────────────────────────────────────── */

/**
 * Which rows the transcript holds, and whether they were delivered while its
 * session was Pending.
 *
 * A Pending session's socket delivers no rows. When its tree opens, the
 * catalog announces the new `stream_state` and the session socket resyncs,
 * on separate sockets, so either can land first. Until rows delivered after
 * the flip replace the empty Pending ones, the transcript keeps its
 * not-imported face rather than "Nothing readable yet".
 */
export interface ImportWatch {
  readonly sessionId: string;
  /** The delivered rows, by identity; null before a snapshot arrives. */
  readonly delivered: readonly unknown[] | null;
  readonly pendingEra: boolean;
}

/** `delivered` must keep its identity until the feed delivers again: pass a
 * feed's rows only once its snapshot is ready, and null before. */
export function watchImport(
  previous: ImportWatch | null, sessionId: string, streamState: StreamState,
  delivered: readonly unknown[] | null,
): ImportWatch {
  if (previous !== null && previous.sessionId === sessionId && previous.delivered === delivered) {
    return previous;
  }
  return { sessionId, delivered, pendingEra: delivered !== null && streamState === "pending" };
}

/** The session has opened, but its rows are still the empty Pending ones. */
export function holdsImport(watch: ImportWatch, streamState: StreamState): boolean {
  return watch.pendingEra && streamState !== "pending" &&
    watch.delivered !== null && watch.delivered.length === 0;
}

/* ── a full command asked for during the import ───────────────────────── */

/**
 * The words a full command's disclosure says when it cannot load.
 *
 * The daemon answers `not_ready` while the archive is unpublished: for a
 * conversation still importing (its message says "importing"), or for one
 * that is open but whose raw record is not saved yet (it says "saving", or
 * names publication). Any other failure, including a `not_ready` that no
 * wait will cure (a server with no raw-ledger reader), keeps the room's own
 * words.
 */
export function commandFailureLabel(error: unknown): string {
  const code = typeof error === "object" && error !== null && "code" in error ? error.code : null;
  if (code !== "not_ready") return "couldn’t load · retry";
  const message = error instanceof Error ? error.message : "";
  if (/\bimport/i.test(message)) return "available when imported · retry";
  return /\b(?:sav|publi)/i.test(message) ? "still saving · retry" : "couldn’t load · retry";
}
