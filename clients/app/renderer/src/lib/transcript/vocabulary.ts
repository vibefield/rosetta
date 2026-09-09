// The ledger's closed vocabulary — §04 of the locked transcript record.
//
// The protocol's `ToolAction` union is closed, so the ledger's grammar is closed
// too: every family projects to `glyph · verb · object · result` and names what
// its line opens into (T-2 — machinery is a sentence, not a count). Every table
// a consumer needs lives HERE and nowhere else, so the run's summary sentence,
// the column head and the lines themselves can never disagree about what a
// `shell` row is called.
//
// Pure: no IO, no clock, no randomness. Safe to import from a test.
import type {
  AttachmentRef,
  CanonicalText,
  CitationRef,
  Notice,
  ResourceRef,
  TimelineEntry,
  ToolAction,
} from "@rosetta/protocol";

/* ── families, classes and nouns ─────────────────────────────────────────── */

/** One of the protocol's twenty-eight closed action families. */
export type ToolFamily = ToolAction["type"];

/**
 * T-3's fold rule. `look` is high-volume and low-stakes, so consecutive looks
 * of one family fold to a single line; `act` is the session's work product and
 * always stands alone. `shell` is deliberately a LOOK (decided 2026-09-01) —
 * running a command is looking at the world, and a *failed* command is an act,
 * which is why a failure breaks the fold.
 */
export type LedgerClass = "look" | "act";

export const FAMILY_CLASS: Readonly<Record<ToolFamily, LedgerClass>> = {
  file_read: "look",
  file_list: "look",
  file_search: "look",
  code_lookup: "look",
  web_search: "look",
  web_fetch: "look",
  web_navigate: "look",
  tool_discovery: "look",
  shell: "look",
  file_change: "act",
  code_execute: "act",
  process: "act",
  orchestration: "act",
  delegate: "act",
  collaborate: "act",
  question: "act",
  skill: "act",
  workflow: "act",
  schedule: "act",
  artifact: "act",
  notification: "act",
  mode: "act",
  task: "act",
  plan: "act",
  finding: "act",
  structured_output: "act",
  external: "act",
  unknown: "act",
};

/**
 * The run's closing sentence is nouns, never verbs — `17 commands · 3 edits ·
 * 4 reads` (T-3). One entry per family, singular and plural.
 */
export const FAMILY_NOUN: Readonly<Record<ToolFamily, readonly [string, string]>> = {
  file_read: ["read", "reads"],
  file_list: ["listing", "listings"],
  file_search: ["search", "searches"],
  code_lookup: ["lookup", "lookups"],
  web_search: ["web search", "web searches"],
  web_fetch: ["fetch", "fetches"],
  web_navigate: ["page", "pages"],
  tool_discovery: ["tool search", "tool searches"],
  shell: ["command", "commands"],
  code_execute: ["run", "runs"],
  file_change: ["edit", "edits"],
  process: ["process", "processes"],
  orchestration: ["tool batch", "tool batches"],
  delegate: ["delegation", "delegations"],
  collaborate: ["message", "messages"],
  question: ["question", "questions"],
  skill: ["skill", "skills"],
  workflow: ["workflow", "workflows"],
  schedule: ["schedule", "schedules"],
  artifact: ["artifact", "artifacts"],
  notification: ["notification", "notifications"],
  mode: ["mode change", "mode changes"],
  task: ["task", "tasks"],
  plan: ["plan", "plans"],
  finding: ["report", "reports"],
  structured_output: ["document", "documents"],
  external: ["call", "calls"],
  unknown: ["call", "calls"],
};

/** `1 read` · `4 reads`. The count is always stated; the noun agrees with it. */
export function familyNoun(family: ToolFamily, count: number): string {
  const [one, many] = FAMILY_NOUN[family];
  return `${count} ${count === 1 ? one : many}`;
}

/**
 * The nouns a *fold* uses for its own members — `4 files`, `2 commands`. A fold
 * counts the things looked at, which is not always the family's run noun: four
 * `file_read` rows are `4 files` on the line and `4 reads` in the sentence.
 */
export const FOLD_NOUN: Readonly<Record<ToolFamily, readonly [string, string]>> = {
  ...FAMILY_NOUN,
  file_read: ["file", "files"],
  file_list: ["directory", "directories"],
  shell: ["command", "commands"],
  web_fetch: ["page", "pages"],
};

/* ── glyphs ──────────────────────────────────────────────────────────────── */

/**
 * Monoline glyph names on the 16-unit grid at stroke 1.5, `currentColor` — the
 * record's own symbol ids minus the `g-` prefix, so a component draws
 * `<use href={"#g-" + glyph}/>`. One glyph per FAMILY, never per operation: a
 * file's type is not a state, and neither is a tool's, so `Created` and
 * `Edited` share the delta glyph and the verb carries the operation.
 *
 * The glyph slot is substituted by STATE, not by this name (§04's last rows):
 * a `failed` line draws `#g-fail`, a `running` line draws the breathing dot.
 */
export type LedgerGlyph =
  | "read"
  | "list"
  | "search"
  | "web"
  | "shell"
  | "delta"
  | "delegate"
  | "task"
  | "question"
  | "doc"
  | "bell"
  | "box"
  | "plug"
  | "flag"
  | "bolt"
  | "sliders"
  | "clock"
  | "help";

export const FAMILY_GLYPH: Readonly<Record<ToolFamily, LedgerGlyph>> = {
  file_read: "read",
  file_list: "list",
  file_search: "search",
  code_lookup: "search",
  web_search: "web",
  web_fetch: "web",
  web_navigate: "web",
  tool_discovery: "plug",
  file_change: "delta",
  shell: "shell",
  code_execute: "shell",
  process: "clock",
  orchestration: "bolt",
  delegate: "delegate",
  collaborate: "bell",
  question: "question",
  skill: "bolt",
  workflow: "bolt",
  schedule: "clock",
  artifact: "box",
  notification: "bell",
  mode: "sliders",
  task: "task",
  plan: "task",
  finding: "flag",
  structured_output: "doc",
  external: "plug",
  unknown: "help",
};

/* ── the object column ───────────────────────────────────────────────────── */

/**
 * Ink and typeface for one span of the object column, named for the record's
 * own classes so a component maps them without translating:
 *
 * - `plain` → a bare span: mono 11.5, primary ink. The object's default.
 * - `dir`   → `.dir`: mono, tertiary. A path's directory, so the NAME is what
 *             the eye lands on.
 * - `query` → `.q`: mono, primary. A search term, quoted by weight not marks.
 * - `aside` → `.in`: SANS 12, tertiary. Qualifiers and prose — `in files`,
 *             a fold's name list, a delegate's objective, a question's prompt.
 */
export type ObjectTone = "plain" | "dir" | "query" | "aside";

export interface ObjectPart {
  readonly text: string;
  readonly tone: ObjectTone;
}

/**
 * Concatenate the parts with NO separator — each part's text already carries
 * the spacing the record draws (a directory ends in `/`, an aside opens with
 * ` · ` or ` `). Use it for a `title` attribute or an assertion, never as a
 * substitute for drawing the parts, which is where the tones live.
 */
export function objectText(parts: readonly ObjectPart[]): string {
  return parts.map((part) => part.text).join("");
}

export function part(text: string, tone: ObjectTone = "plain"): ObjectPart {
  return { text, tone };
}

/* ── paths, text and labels ──────────────────────────────────────────────── */

/**
 * Where the reading happens. A path is drawn relative to the session's own
 * working directory when it lies beneath it, then home-relative, then whole —
 * the record draws `packages/field-app/src/design-system/CardShell.tsx`, never
 * the eleven leading characters that are the same on every line.
 */
export interface LedgerContext {
  /** `SessionRow.cwd` — the adapter's one settled primary working directory. */
  readonly cwd?: string | null;
  /** The viewer's home directory, for the `~/…` form. */
  readonly home?: string | null;
}

export function shortenPath(path: string, context: LedgerContext = {}): string {
  const cwd = context.cwd?.replace(/\/+$/, "") ?? null;
  if (cwd !== null && cwd.length > 0) {
    if (path === cwd) return basename(path);
    if (path.startsWith(`${cwd}/`)) return path.slice(cwd.length + 1);
  }
  const home = context.home?.replace(/\/+$/, "") ?? null;
  if (home !== null && home.length > 0 && path.startsWith(`${home}/`)) {
    return `~/${path.slice(home.length + 1)}`;
  }
  return path;
}

export function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  const cut = trimmed.lastIndexOf("/");
  return cut === -1 ? trimmed : trimmed.slice(cut + 1);
}

/**
 * Split a path or url into its dim leading run and its bright last segment.
 * A url loses its scheme first: the record draws
 * `developer.mozilla.org/en-US/docs/Web/API/` + `WebGPU_API`.
 */
export function splitPath(label: string): { dir: string; name: string } {
  const stripped = label.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "").replace(/\/+$/, "");
  const cut = stripped.lastIndexOf("/");
  if (cut === -1) return { dir: "", name: stripped };
  return { dir: stripped.slice(0, cut + 1), name: stripped.slice(cut + 1) };
}

/** Object parts for a path: directory dim, name bright (§04, `file_read`). */
export function pathParts(label: string): ObjectPart[] {
  const { dir, name } = splitPath(label);
  const parts: ObjectPart[] = [];
  if (dir) parts.push(part(dir, "dir"));
  parts.push(part(name || label));
  return parts;
}

/** The host of a url, for a source's caption and its icon's cache key. */
export function urlHost(url: string): string {
  const match = /^(?:[a-z][a-z0-9+.-]*:\/\/)?([^/?#]+)/i.exec(url);
  return match ? match[1].replace(/^www\./i, "") : url;
}

/** Inline text, or the preview a content-addressed member stands in for. */
export function canonicalText(text: CanonicalText): string {
  return text.type === "inline" ? text.text : text.preview;
}

/** The first line, clamped — every ledger line is one line by construction. */
export function firstLine(text: string, max = 120): string {
  const line = text.split("\n", 1)[0].trim();
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

export function resourceLabel(resource: ResourceRef): string {
  switch (resource.type) {
    case "file":
    case "directory":
      return resource.path;
    case "url":
      return resource.url;
    case "session":
    case "task":
    case "process":
      return resource.id;
    case "asset":
      return resource.asset.url;
  }
}

/** A resource as the reader should see it: shortened when it is a local path. */
export function resourceDisplay(resource: ResourceRef, context: LedgerContext): string {
  const label = resourceLabel(resource);
  return resource.type === "file" || resource.type === "directory"
    ? shortenPath(label, context)
    : label;
}

export function attachmentLabel(attachment: AttachmentRef): string {
  return attachment.name?.trim() || attachment.label?.trim() || (attachment.kind === "image" ? "Image" : "Attachment");
}

export function citationLabel(citation: CitationRef): string {
  const source = citation.source ? resourceLabel(citation.source) : null;
  return citation.title?.trim() || source || citation.provider?.trim() || "Source";
}

/**
 * Every `Notice` is a hairline marker, never a filled alarm (§03 G). This is
 * its one line of text; a `runtime_error` earns red text and nothing more.
 */
export function noticeText(notice: Notice): string {
  switch (notice.type) {
    case "compaction":
      return notice.reason ? `context compacted · ${notice.reason}` : "context compacted";
    case "truncation": {
      const count = notice.omitted_count === null ? "" : ` · ${notice.omitted_count} omitted`;
      return `${notice.scope.replaceAll("_", " ")} truncated${count}`;
    }
    case "model_changed":
      return notice.from ? `model ${notice.from} → ${notice.to}` : `model → ${notice.to}`;
    case "mode_changed":
      return notice.from ? `mode ${notice.from} → ${notice.to}` : `mode → ${notice.to}`;
    case "permission_requested":
      return notice.scope ? `permission requested · ${notice.scope}` : "permission requested";
    case "permission_decision":
      return notice.scope
        ? `permission ${notice.decision} · ${notice.scope}`
        : `permission ${notice.decision}`;
    case "handoff":
      return `handed off to ${notice.to_agent ?? notice.to_session_id ?? "another agent"}`;
    case "command_output":
      return canonicalText(notice.output);
    case "async_task": {
      const identity = notice.task_id ?? notice.tool_use_id;
      const status = notice.status === "succeeded" ? "completed" : notice.status.replaceAll("_", " ");
      return ["background task", identity, status, notice.summary]
        .filter((value) => value !== null)
        .join(" · ");
    }
    case "runtime_error":
      return `runtime error · ${notice.kind.replaceAll("_", " ")} · ${notice.summary}`;
  }
}

/* ── spans ───────────────────────────────────────────────────────────────── */

/**
 * A span at second precision — `21s`, `9m 36s`, `1h 04m`, `1d 11h`. The run's
 * sentence and a line's own duration both use it. `lib/format.ts`'s `duration`
 * is the canonical MINUTE-precision form and is what §07's pause pill uses;
 * this is its finer sibling, because a two-command fold that took 21 seconds
 * must not read `0m`.
 */
export function spanText(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${String(seconds % 60).padStart(2, "0")}s`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

/* ── stamps ──────────────────────────────────────────────────────────────── */

/**
 * Every stamp a row can honestly offer, in no particular order. Timing is
 * evidence-backed and frequently absent — the Claude Code and Codex rows on
 * today's daemon carry `completed_at` and nothing else — so a span is taken
 * from what is there and is `null` when the row cannot prove one.
 */
export function rowStamps(row: TimelineEntry): number[] {
  const stamps: number[] = [];
  if (row.at !== null) stamps.push(row.at);
  if (row.body.type === "tool_use") {
    const timing = row.body.tool.timing;
    if (timing) {
      if (timing.started_at !== null) stamps.push(timing.started_at);
      if (timing.completed_at !== null) stamps.push(timing.completed_at);
    }
  }
  return stamps;
}

/** The span across a set of rows, or null when fewer than two stamps exist. */
export function stampSpan(rows: readonly TimelineEntry[]): number | null {
  const stamps = rows.flatMap(rowStamps);
  if (stamps.length < 2) return null;
  const span = Math.max(...stamps) - Math.min(...stamps);
  return span > 0 ? span : null;
}
