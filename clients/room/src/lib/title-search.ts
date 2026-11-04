// Title search over the metadata the room already holds — what the sheet can
// honestly offer while full-text search cannot answer. During a first import
// the projects and session titles are public within seconds, but the
// full-text index answers nothing until the import has finished and its first
// generation is built; until then every query would come back empty, and an
// empty list reads as "nothing matched".
//
// This is not a second search engine. It never ranks beyond the session
// list's own order (newest known activity first), it reads only titles the
// room holds, and the sheet consults it only while full text is unavailable:
// once the index is ready the sheet behaves exactly as before.
import type { SearchReadiness, SearchResponse, SessionRow } from "@vibefield/rosetta-protocol";
import type { ProjectViewModel, SnippetPart } from "./model";
import { projectKey } from "./model";
import { compareSessionActivity } from "./session-activity";

/** The line the sheet prints under title matches during the import. */
export const FULL_TEXT_PENDING = "Full-text search arrives when the import finishes.";
/** …after publication, while the index builds its first generation. */
export const FULL_TEXT_BUILDING = "Full-text search arrives when its index is built.";
/** …and when the index failed. */
export const FULL_TEXT_FAILED = "Full-text search is unavailable.";

/** The line under title matches: what full text is waiting for, never a
 * time. Before publication the import is the gate; after it, the index. */
export function titlesNote(readiness: SearchReadiness | null, canonicalReady: boolean | null): string {
  if (canonicalReady === false) return FULL_TEXT_PENDING;
  return readiness === "failed" ? FULL_TEXT_FAILED : FULL_TEXT_BUILDING;
}

/** How many titles the sheet can search: top-level conversations that have a
 * name to match. Metadata-only placeholders carry none, so they never count
 * — the same unit the import card counts in (conversations). */
export function searchableTitleCount(sessions: readonly SessionRow[]): number {
  let count = 0;
  for (const session of sessions) {
    if (session.parent_session_id === null && sessionTitleText(session) !== null) count += 1;
  }
  return count;
}

/** How many title matches the sheet lists; the count says when there are more. */
export const TITLE_MATCH_LIMIT = 50;

/**
 * Full text cannot answer: the canonical projection is unpublished (the
 * daemon answers every query with zero hits and `building`), the detached
 * index has no generation yet (`building`), or the index failed (it serves
 * no hits; crates/search/src/sidecar.rs `serves_hits`). A catching-up index
 * does answer — partially — and keeps the ordinary sheet. Unknown (null) is
 * no claim either way.
 */
export function fullTextUnavailable(
  readiness: SearchReadiness | null,
  canonicalReady: boolean | null,
): boolean {
  return canonicalReady === false || readiness === "building" || readiness === "failed";
}

/**
 * Titles stand in while full text cannot answer, and after it can, until
 * the answer on screen is one full text gave. The zero hits an unpublished
 * index answered with (`building`) would otherwise read as "nothing matched"
 * in the moment between the import finishing and the re-asked query landing.
 */
export function titlesStandIn(
  readiness: SearchReadiness | null,
  canonicalReady: boolean | null,
  response: SearchResponse | null,
): boolean {
  return fullTextUnavailable(readiness, canonicalReady) || response?.state.readiness === "building";
}

/* ── folding: case- and accent-insensitive, the way the index tokenizes ── */

const ASCII = /^[\x00-\x7f]*$/;
const MARKS = /\p{M}+/gu;

function foldChar(ch: string): string {
  return ch.normalize("NFKD").replace(MARKS, "").toLowerCase();
}

function foldUncached(text: string): string {
  if (ASCII.test(text)) return text.toLowerCase();
  // Code point by code point, so a match maps back onto the original title
  // (`markTitle`) through exactly the same fold.
  let out = "";
  for (const ch of text) out += foldChar(ch);
  return out;
}

// Titles repeat across every metadata update of an import; folding is a pure
// function of the text, so a bounded memo keeps keystrokes O(sessions) cheap.
const FOLD_CACHE_CAP = 65_536;
const folded = new Map<string, string>();

/** Lower-cased, accents and other combining marks removed. */
export function foldText(text: string): string {
  const hit = folded.get(text);
  if (hit !== undefined) return hit;
  const value = foldUncached(text);
  if (folded.size >= FOLD_CACHE_CAP) folded.clear();
  folded.set(text, value);
  return value;
}

/**
 * The query's terms, folded: each word, and a quoted phrase as one term —
 * the sheet's printed grammar ("every word must match", quotes for a
 * phrase), read loosely. A quote left open is just part of a word here.
 * Duplicates fold away; order is kept.
 */
export function titleTerms(query: string): string[] {
  const terms: string[] = [];
  for (const match of query.matchAll(/"([^"]*)"|(\S+)/g)) {
    const raw = match[1] ?? match[2]!.replace(/^"+|"+$/g, "");
    const term = foldText(raw.trim());
    if (term !== "" && !terms.includes(term)) terms.push(term);
  }
  return terms;
}

/** The name a session's row shows, when it has one to search. */
export function sessionTitleText(session: SessionRow): string | null {
  return session.title ?? session.summary;
}

/**
 * The title as parts, every occurrence of every term marked — the same
 * shape `parseSnippet` gives a full-text hit, so both draw alike.
 */
export function markTitle(title: string, terms: readonly string[]): SnippetPart[] {
  let foldedTitle = "";
  const from: number[] = [];
  const to: number[] = [];
  for (let at = 0; at < title.length; ) {
    const ch = String.fromCodePoint(title.codePointAt(at)!);
    const fold = ASCII.test(ch) ? ch.toLowerCase() : foldChar(ch);
    for (let unit = 0; unit < fold.length; unit += 1) {
      from.push(at);
      to.push(at + ch.length);
    }
    foldedTitle += fold;
    at += ch.length;
  }
  const ranges: [number, number][] = [];
  for (const term of terms) {
    for (let at = foldedTitle.indexOf(term); at !== -1; at = foldedTitle.indexOf(term, at + 1)) {
      ranges.push([from[at]!, to[at + term.length - 1]!]);
    }
  }
  ranges.sort((left, right) => left[0] - right[0] || left[1] - right[1]);
  const merged: [number, number][] = [];
  for (const range of ranges) {
    const last = merged[merged.length - 1];
    if (last !== undefined && range[0] <= last[1]) last[1] = Math.max(last[1], range[1]);
    else merged.push([range[0], range[1]]);
  }
  const parts: SnippetPart[] = [];
  let cursor = 0;
  for (const [start, end] of merged) {
    if (start > cursor) parts.push({ text: title.slice(cursor, start), mark: false });
    parts.push({ text: title.slice(start, end), mark: true });
    cursor = end;
  }
  if (cursor < title.length) parts.push({ text: title.slice(cursor), mark: false });
  return parts;
}

/* ── the matches ───────────────────────────────────────────────────────── */

export interface TitleMatch {
  /** The DOM id `aria-activedescendant` points at. */
  readonly id: string;
  /** Position in the walkable list. */
  readonly index: number;
  /** A top-level thread — what opening navigates to. */
  readonly session: SessionRow;
  readonly project: ProjectViewModel;
  readonly parts: readonly SnippetPart[];
}

export interface TitleMatches {
  readonly matches: readonly TitleMatch[];
  /** Every thread whose title matched, before the cap. */
  readonly total: number;
}

/** The sheet's own narrowing, applied to titles. The kind chip (prompts,
 * replies) narrows messages; a title is neither, so it does not apply. */
export interface TitleScope {
  /** A canonical project id, or null for everywhere. */
  readonly project: string | null;
  /** A runtime family (`claude-code`, …), or null for every agent. */
  readonly agent: string | null;
  readonly runtimeKind: (runtimeId: string) => string;
}

export function titleOptionId(sessionId: string): string {
  return `ah-sheet-title-${sessionId}`;
}

/**
 * Top-level threads whose title contains every term, newest known activity
 * first (the session list's own tie-breaks), capped at `limit`. Threads in a
 * project the room does not show are left out, as full-text hits are.
 */
export function matchSessionTitles(
  query: string,
  sessions: readonly SessionRow[],
  projectsById: ReadonlyMap<string, ProjectViewModel>,
  scope: TitleScope,
  limit = TITLE_MATCH_LIMIT,
): TitleMatches {
  const terms = titleTerms(query);
  if (terms.length === 0) return { matches: [], total: 0 };
  const found: { session: SessionRow; project: ProjectViewModel; title: string }[] = [];
  for (const session of sessions) {
    if (session.parent_session_id !== null) continue;
    const title = sessionTitleText(session);
    if (title === null) continue;
    const key = projectKey(session);
    if (scope.project !== null && key !== scope.project) continue;
    if (scope.agent !== null && scope.runtimeKind(session.runtime_id) !== scope.agent) continue;
    const text = foldText(title);
    if (!terms.every((term) => text.includes(term))) continue;
    const project = projectsById.get(key);
    if (project === undefined) continue;
    found.push({ session, project, title });
  }
  found.sort((left, right) => compareSessionActivity(left.session, right.session));
  const matches = found.slice(0, Math.max(0, limit)).map(({ session, project, title }, index) => ({
    id: titleOptionId(session.id),
    index,
    session,
    project,
    parts: markTitle(title, terms),
  }));
  return { matches, total: found.length };
}

/** `3 titles`, or `first 50 of 132 titles` when the list is capped. */
export function titleCountLabel(result: TitleMatches): string {
  const total = result.total.toLocaleString("en-US");
  const noun = result.total === 1 ? "title" : "titles";
  return result.matches.length < result.total
    ? `first ${result.matches.length.toLocaleString("en-US")} of ${total} ${noun}`
    : `${total} ${noun}`;
}
