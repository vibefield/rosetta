// The search sheet's view model — everything the sheet decides without a DOM.
//
// The sheet is a tool held over the room, not a mode that replaces it (design
// record, search §06 as revised 2026-09-09): the reader summons it, picks a
// row, and the room is exactly where they left it. What lives here is pure:
// the flat line list a listbox walks, the keyboard cursor's arithmetic, the
// snippet cleaned the way the ledger prints a path, the honest count sentence,
// and the memory of what was searched before. Rendering is `SearchSheet.tsx`.
import type { SearchHit, SearchReadiness, SessionRow } from "@rosetta/protocol";
import type { ProjectViewModel } from "./model";
import type {
  SearchResultCounts,
  SearchResultsViewModel,
  SessionSearchResult,
} from "./search-results";

/* ── filters — the chips over the list, mapped 1:1 onto the API ────────── */

/** The canonical wire kinds the sheet lets the reader narrow to. `any` sends
 * no `kind`; the two named ones are exactly what search indexes (prompts and
 * replies, decided 2026-09-09) — the vocabulary a reader thinks in: what I
 * asked, what it said. */
export type SearchKindFilter = "any" | "user" | "agent";
export type SearchScopeFilter = "everywhere" | "project";

export interface SearchFilters {
  readonly scope: SearchScopeFilter;
  readonly kind: SearchKindFilter;
  /** A runtime family (`claude-code`, `codex`, …), or null for every agent. */
  readonly agent: string | null;
}

export const DEFAULT_SEARCH_FILTERS: SearchFilters = {
  scope: "everywhere",
  kind: "any",
  agent: null,
};

export const KIND_FILTER_OPTIONS: readonly { value: SearchKindFilter; label: string }[] = [
  { value: "any", label: "prompts and replies" },
  { value: "user", label: "prompts" },
  { value: "agent", label: "replies" },
];

/* ── the flat list a listbox walks ─────────────────────────────────────── */

export interface SheetHead {
  readonly type: "head";
  readonly key: string;
  readonly project: ProjectViewModel;
  readonly session: SessionRow;
  readonly hitCount: number;
}

export interface SheetOption {
  readonly type: "option";
  readonly key: string;
  /** The DOM id `aria-activedescendant` points at. */
  readonly id: string;
  /** The option's position among options only — heads are not walkable. */
  readonly index: number;
  readonly project: ProjectViewModel;
  /** The navigable root session (a child hit lands inside its parent). */
  readonly session: SessionRow;
  readonly hit: SearchHit;
}

export type SheetLine = SheetHead | SheetOption;

export interface SheetLines {
  readonly lines: readonly SheetLine[];
  readonly options: readonly SheetOption[];
}

export function optionDomId(hit: SearchHit): string {
  return `ah-sheet-opt-${hit.session_id}-${hit.timeline_id}`;
}

/** One session's hits in reading order: by time when the source reported one,
 * then by id, so the stepper walks the transcript downward. */
export function hitsInReadingOrder(hits: readonly SearchHit[]): SearchHit[] {
  return [...hits].sort(
    (a, b) =>
      (a.at ?? Number.MAX_SAFE_INTEGER) - (b.at ?? Number.MAX_SAFE_INTEGER) ||
      a.timeline_id.localeCompare(b.timeline_id),
  );
}

/**
 * Flattens the grouped result model into the lines the sheet draws: one
 * sticky head per session, its hits beneath in reading order. Project is
 * folded into the head's breadcrumb rather than being a level of its own —
 * three levels of nesting in a 640px sheet is a tree, and a tree is not
 * something you scan.
 */
export function buildSheetLines(model: SearchResultsViewModel | null): SheetLines {
  if (model === null) return { lines: [], options: [] };
  const lines: SheetLine[] = [];
  const options: SheetOption[] = [];
  for (const group of model.groups) {
    for (const result of group.sessions) {
      lines.push({
        type: "head",
        key: `head:${result.session.id}`,
        project: group.project,
        session: result.session,
        hitCount: result.hits.length,
      });
      for (const hit of hitsInReadingOrder(result.hits)) {
        const option: SheetOption = {
          type: "option",
          key: `opt:${hit.session_id}:${hit.timeline_id}`,
          id: optionDomId(hit),
          index: options.length,
          project: group.project,
          session: result.session,
          hit,
        };
        options.push(option);
        lines.push(option);
      }
    }
  }
  return { lines, options };
}

/** Find the session result that owns a hit, for the stepper. */
export function sessionResultOf(
  model: SearchResultsViewModel,
  rootSessionId: string,
): SessionSearchResult | null {
  for (const group of model.groups) {
    for (const result of group.sessions) {
      if (result.session.id === rootSessionId) return result;
    }
  }
  return null;
}

/* ── the keyboard cursor ───────────────────────────────────────────────── */

export type CursorKey = "ArrowDown" | "ArrowUp" | "Home" | "End" | "PageDown" | "PageUp";

export function isCursorKey(key: string): key is CursorKey {
  return (
    key === "ArrowDown" ||
    key === "ArrowUp" ||
    key === "Home" ||
    key === "End" ||
    key === "PageDown" ||
    key === "PageUp"
  );
}

/** APG listbox arithmetic: no wrap, clamped at both ends, a page of eight. */
export function moveCursor(active: number, key: CursorKey, count: number, page = 8): number {
  if (count <= 0) return -1;
  const last = count - 1;
  const from = active < 0 ? -1 : Math.min(active, last);
  switch (key) {
    case "ArrowDown":
      return Math.min(last, from + 1);
    case "ArrowUp":
      return Math.max(0, from - 1);
    case "Home":
      return 0;
    case "End":
      return last;
    case "PageDown":
      return Math.min(last, Math.max(0, from) + page);
    case "PageUp":
      return Math.max(0, from - page);
  }
}

/** Jump to the first option of the next session (or of the previous one):
 * VS Code's sticky separators and Raycast's sections both give the keyboard a
 * way past a long group without holding an arrow. Clamps at the ends. */
export function moveSection(
  active: number,
  direction: 1 | -1,
  options: readonly SheetOption[],
): number {
  if (options.length === 0) return -1;
  const from = active < 0 ? 0 : Math.min(active, options.length - 1);
  // Every index read here stays inside options: from is clamped to it, the
  // walk down stops above 0, and the walk up below options.length.
  const sessionAt = (index: number): string => options[index]!.session.id;
  const current = sessionAt(from);
  if (direction === 1) {
    for (let i = from + 1; i < options.length; i += 1) {
      if (sessionAt(i) !== current) return i;
    }
    return options.length - 1;
  }
  // Up: the head of this session if we are not already on it, else the
  // head of the session before.
  let head = from;
  while (head > 0 && sessionAt(head - 1) === current) head -= 1;
  if (head !== from) return head;
  if (head === 0) return 0;
  const previous = sessionAt(head - 1);
  let start = head - 1;
  while (start > 0 && sessionAt(start - 1) === previous) start -= 1;
  return start;
}

/* ── the words on a row ────────────────────────────────────────────────── */

/**
 * The snippet as the sheet prints it: whitespace collapsed to one line of
 * prose, and every home-rooted path shortened to `~/…` — the ledger row's own
 * rule, so a path reads the same in a result as it does in the transcript.
 * The `[` `]` match markers survive untouched for `parseSnippet`.
 */
export function cleanSnippet(snippet: string, home: string | null): string {
  let text = snippet.replace(/\s+/g, " ").trim();
  if (home !== null && home !== "" && home !== "/") {
    const escaped = home.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    text = text.replace(new RegExp(`(^|[^\\w~])${escaped}(?=/|\\b)`, "g"), "$1~");
  }
  return text;
}

/** Who said it, in the room's own words. */
export function kindWord(kind: string, agentLabel: string): string {
  switch (kind) {
    case "user":
      return "you";
    case "agent":
      return agentLabel;
    case "tool_use":
      return "activity";
    case "thinking":
      return "thinking";
    case "summary":
      return "summary";
    case "notice":
      return "notice";
    case "collaboration":
      return "team";
    default:
      return kind;
  }
}

const DAY_MS = 86_400_000;

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** When the matched message was written: the clock alone inside today, the
 * day and clock inside the year, the day and year beyond it; a dash when the
 * source never said. Every part is tabular by the stylesheet. */
export function hitWhen(at: number | null, nowMs: number): string {
  if (at === null) return "—";
  const d = new Date(at);
  const p = (v: number) => String(v).padStart(2, "0");
  const clock = `${p(d.getHours())}:${p(d.getMinutes())}`;
  const today = startOfDay(nowMs);
  if (at >= today && at < today + DAY_MS) return clock;
  const day = d.toLocaleDateString("en-GB", { day: "numeric", month: "short" });
  if (d.getFullYear() === new Date(nowMs).getFullYear()) return `${day} · ${clock}`;
  return `${day} ${d.getFullYear()}`;
}

/** A session's name for a breadcrumb: its title, its summary, or — for a
 * session nobody named — the first eight characters of its native id, which
 * the caller sets in mono. `mono` says which happened. */
export function sessionCrumb(session: SessionRow): { text: string; mono: boolean } {
  if (session.title !== null) return { text: session.title, mono: false };
  if (session.summary !== null) return { text: session.summary, mono: false };
  return { text: session.native_session_id.slice(0, 8), mono: true };
}

function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;
}

/** `43 matches · 12 sessions · 4 projects` — the numbers, never the word
 * "results", which hides its own reach (design record §06). */
export function countSentence(counts: SearchResultCounts): string {
  return [
    plural(counts.matches, "match", "matches"),
    plural(counts.sessions, "session"),
    plural(counts.projects, "project"),
  ].join(" · ");
}

export interface SheetStateInput {
  readonly query: string;
  readonly error: string | null;
  readonly readiness: SearchReadiness | null;
  readonly progress: { documents_done: number; documents_total: number } | null;
  readonly canonicalReady: boolean | null;
  readonly ingestPaused: boolean;
  readonly counts: SearchResultCounts | null;
  readonly limit: number;
}

/**
 * The one sentence in the sheet's foot. Degraded is Tuesday here (design
 * record §09): a building index says so with a number, an error says what
 * happened, and a full page says it is the first page.
 */
export function stateSentence(input: SheetStateInput): string {
  if (input.error !== null) return `search failed · ${input.error}`;
  if (input.ingestPaused) return "transcript import paused · search covers what was read";
  if (input.readiness !== null && input.readiness !== "ready") {
    if (input.readiness === "failed") {
      return input.canonicalReady ? "search unavailable" : "history is being imported";
    }
    if (input.progress !== null && input.progress.documents_total > 0) {
      const pct = Math.floor((100 * input.progress.documents_done) / input.progress.documents_total);
      return `indexing · ${pct}% · ${input.progress.documents_done.toLocaleString("en-US")} of ${input.progress.documents_total.toLocaleString("en-US")} messages · results are partial`;
    }
    if (input.readiness === "building") {
      return input.canonicalReady ? "search is being prepared" : "history is being imported";
    }
    return `search is ${input.readiness.replace("-", " ")} · results may be partial`;
  }
  if (input.counts === null || input.query === "") return "";
  const sentence = countSentence(input.counts);
  return input.counts.matches >= input.limit
    ? `first ${input.limit} matches · narrow the search to see the rest`
    : sentence;
}

/* ── what was searched before ──────────────────────────────────────────── */

/** Under the room's storage prefix. */
export const RECENT_QUERIES_KEY = "search.recent";
export const RECENT_QUERIES_CAP = 8;

export interface RecentStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export function readRecentQueries(store: RecentStore | null): string[] {
  if (store === null) return [];
  try {
    const raw = store.getItem(RECENT_QUERIES_KEY);
    if (raw === null) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((q): q is string => typeof q === "string" && q.trim() !== "");
  } catch {
    return [];
  }
}

function writeRecent(store: RecentStore | null, queries: string[]): string[] {
  if (store !== null) {
    try {
      store.setItem(RECENT_QUERIES_KEY, JSON.stringify(queries));
    } catch {
      // A browser that refuses storage refuses the memory, not the search.
    }
  }
  return queries;
}

/** Most recent first, exact duplicates folded, capped. */
export function rememberQuery(store: RecentStore | null, query: string): string[] {
  const q = query.trim();
  if (q === "") return readRecentQueries(store);
  const rest = readRecentQueries(store).filter((r) => r !== q);
  return writeRecent(store, [q, ...rest].slice(0, RECENT_QUERIES_CAP));
}

export function forgetQuery(store: RecentStore | null, query: string): string[] {
  return writeRecent(
    store,
    readRecentQueries(store).filter((r) => r !== query),
  );
}
