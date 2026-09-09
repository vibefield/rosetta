// The ledger — a contiguous run of tool rows as lines you can read without
// opening (T-2), folded by class (T-3), with hue spent only on what did not
// complete or is still happening (T-5).
//
// ── The fold rules, exactly as implemented ───────────────────────────────────
//
//  1. Consecutive lines fold into one when ALL of:
//       · the family's class is `look` (T-3: looking is high-volume and
//         low-stakes; acts are the session's work product and never fold),
//       · the family is the SAME (a read and a search are two sentences, not
//         one — `Read · 4 files` and `Ran · 3 commands` are the record's own
//         two drawings of a fold),
//       · the line's state is `done`. A failure BREAKS the fold and stands
//         alone in red (T-3 states this for commands; a failed look is an act
//         by the same argument, and §06 draws a failed search standing alone).
//         `running` and `muted` break it too — a fold cannot honestly report a
//         state its members do not share.
//  2. A group of one is not a fold; it stays the line it already was.
//  3. `shell` is in the LOOK class and folds (decided 2026-09-01).
//     `code_execute` is an ACT in §04's table and does NOT fold, even though
//     its verb is also `Ran`: running code you wrote is a work product.
//  4. A fold names its members — `Ran · 3 commands · ls, cat, grep` — up to
//     FOLD_NAME_LIMIT of them. Past that the object degrades to the record's
//     own `… 12 more commands` and the time slot carries the fold's LAST stamp
//     (the component draws the `→` prefix; `elided` says so).
//  5. A run — everything the agent did between two things it said; `turns.ts`
//     cuts it — closes by default once it has RUN_FOLD_THRESHOLD members
//     (lines, and the thoughts, notices and summaries between them), including
//     a single tool line. Closed, the run is one sentence of
//     NOUNS — never of verbs — acts first, the changed files named, the
//     failure count at the right (transcript-fold, locked 2026-09-08). See
//     `runSummary`.
import type {
  StatusBasis,
  TimelineEntry,
  ToolAction,
  ToolInteraction,
  ToolStatus,
} from "@rosetta/protocol";
import type { CommandFact, LedgerDetail, PathFact } from "./families.ts";
import { projectAction } from "./families.ts";
import type {
  LedgerClass,
  LedgerContext,
  LedgerGlyph,
  ObjectPart,
  ToolFamily,
} from "./vocabulary.ts";
import {
  FAMILY_CLASS,
  FOLD_NOUN,
  basename,
  canonicalText,
  familyNoun,
  firstLine,
  part,
  resourceLabel,
  rowStamps,
  spanText,
  stampSpan,
  urlHost,
} from "./vocabulary.ts";

/** Even a single activity line closes to its head (2026-09-09 amendment). */
export const RUN_FOLD_THRESHOLD = 1;

/** How many members a fold names before it degrades to `… N more`. */
export const FOLD_NAME_LIMIT = 4;

/**
 * T-5. `failed` is red (failed · denied · timed out — the act did not
 * complete). `muted` is not red, because nothing failed: cancelled,
 * interrupted, and a status the source never proved. `running` is the green
 * breath, and only a live session should draw it as alive. Success is the
 * absence of all three.
 */
export type LedgerState = "done" | "running" | "failed" | "muted";

/**
 * How the result slot is drawn. `plain` and `muted` are bare spans; `failed`
 * and `running` are the record's `.why` span, which takes its hue from the
 * line's state.
 */
export type ResultTone = "plain" | "failed" | "muted" | "running";

export interface LedgerLine {
  /** Stable across renders: the first member row's `timeline_id`. */
  readonly key: string;
  /** Every row behind this line — one, or a fold's members. Anchors, focus
   * landing and the watermark all read `timeline_id` / `ordinal` from here. */
  readonly rows: readonly TimelineEntry[];
  readonly family: ToolFamily;
  readonly cls: LedgerClass;
  /** The FAMILY glyph. A `failed` line draws `#g-fail` in this slot instead,
   * and a `running` line draws the breathing dot — state substitutes, the
   * name does not change. */
  readonly glyph: LedgerGlyph;
  readonly verb: string;
  /** Concatenate in order with no separator; each part carries its own
   * spacing. `tone` names the record's own class: plain · dir · query · aside. */
  readonly object: readonly ObjectPart[];
  readonly result: string | null;
  readonly resultTone: ResultTone;
  readonly state: LedgerState;
  /** Time-of-day for the margin anchor: `completed_at ?? row.at`, and a fold's
   * first stamp — or its LAST when `elided`. */
  readonly at: number | null;
  readonly startedAt: number | null;
  readonly endedAt: number | null;
  /** The line's own span when the source proved one. When `result` is null the
   * projection has already put this in `result` as `21s`. */
  readonly durationMs: number | null;
  readonly detail: LedgerDetail;
  /** §04: a question, a finding, a structured document — and any failure whose
   * diagnostic is longer than the one phrase the result slot can hold. */
  readonly openByDefault: boolean;
  readonly folded: boolean;
  /** Members in the fold; 1 for a plain line. */
  readonly foldCount: number;
  /** The fold gave up naming its members; draw `→` before `at`. */
  readonly elided: boolean;
  /** Proven child sessions this line owns — the component resolves each to a
   * subthread drawn under the line (§03 F). */
  readonly childSessionIds: readonly string[];
  /** The bounded failure text, when there is one. Never the tool's output. */
  readonly diagnostic: string | null;
}

/* ── one row ─────────────────────────────────────────────────────────────── */

function toolOf(row: TimelineEntry): ToolInteraction {
  if (row.body.type !== "tool_use") {
    throw new Error(`timeline entry ${row.timeline_id} is not a tool use`);
  }
  return row.body.tool;
}

function stateOf(status: ToolStatus, basis: StatusBasis): LedgerState {
  switch (status) {
    case "failed":
    case "denied":
    case "timed_out":
      return "failed";
    case "cancelled":
    case "interrupted":
      return "muted";
    case "unknown":
      // A terminal record proves that the call finished even when its native
      // format omitted success/failure. Treat that as neutral completion; it
      // must not look live, failed, or like a warning.
      return basis === "native_terminal_record" ? "done" : "muted";
    case "pending":
    case "running":
      return "running";
    case "succeeded":
      return "done";
  }
}

/** `timed out` · `status unknown`. Proven completion says nothing at all. */
function statusWord(status: ToolStatus, basis: StatusBasis): string | null {
  if (status === "succeeded") return null;
  if (status === "unknown") {
    return basis === "native_terminal_record" ? null : "status unknown";
  }
  return status.replaceAll("_", " ");
}

/**
 * The failure's own words, which always beat the family's outcome — Grok
 * reporting `found 0 matches` as a failure is drawn in red with the source's
 * sentence, not with the vocabulary's cheerful `found nothing`.
 */
function failureResult(tool: ToolInteraction, action: ToolAction): string | null {
  const text = tool.failure?.summary ?? tool.failure?.diagnostic ?? null;
  const line = text === null ? null : firstLine(text, 60);
  const timedOut = tool.status.value === "timed_out" || tool.failure?.kind === "timeout";
  if ((action.type === "web_fetch" || action.type === "web_navigate") && !timedOut) {
    const status = action.outcome?.http_status;
    if (status != null && status >= 400) return `HTTP ${status}`;
  }
  if (action.type === "shell" || action.type === "code_execute") {
    if (timedOut) {
      // The BUDGET, in the seconds the caller set it in — `timed out · 120s`
      // (§04). It is a limit that was hit, not a span that was measured.
      const budget = action.type === "shell" ? action.timeout_ms : null;
      return budget === null ? "timed out" : `timed out · ${Math.round(budget / 1000)}s`;
    }
    const exit = action.outcome?.exit_code ?? null;
    const code = exit === null || exit === 0 ? null : `exit ${exit}`;
    if (code === null) return line;
    return line === null ? code : `${code} · ${line}`;
  }
  if (timedOut && line === null) return "timed out";
  return line;
}

function childSessionIdsOf(action: ToolAction): string[] {
  switch (action.type) {
    case "delegate":
      return action.child_session_id === null ? [] : [action.child_session_id];
    case "collaborate":
      return action.linked_session_id === null ? [] : [action.linked_session_id];
    case "workflow":
      return action.run == null
        ? []
        : [...new Set(action.run.workers.flatMap((worker) =>
            worker.child_session_id === null ? [] : [worker.child_session_id],
          ))];
    default:
      return [];
  }
}

function workflowState(action: ToolAction, fallback: LedgerState): LedgerState {
  if (action.type !== "workflow" || action.run == null) return fallback;
  switch (action.run.status) {
    case "completed":
      return "done";
    case "running":
      return "running";
    case "failed":
      return "failed";
    case "killed":
    case "cancelled":
    case "stopped":
    case "unknown":
      return "muted";
  }
}

/** Every proven child session a row owns — the same fact `Transcript.tsx`
 * already places subthreads by, exported so the component keeps one source. */
export function linkedChildSessionIds(row: TimelineEntry): readonly string[] {
  return row.body.type === "tool_use" ? childSessionIdsOf(row.body.tool.action) : [];
}

/**
 * One `tool_use` row as one ledger line: the §04 sentence, wrapped in the
 * evidence the row actually carries. Throws if the row is not a tool use —
 * `documentTurns` never hands it anything else.
 */
export function ledgerLine(row: TimelineEntry, context: LedgerContext = {}): LedgerLine {
  const tool = toolOf(row);
  const action = tool.action;
  const projection = projectAction(action, tool.native, context, {
    sessionId: row.session_id,
    timelineId: row.timeline_id,
  });
  const status = tool.status.value;
  const statusBasis = tool.status.basis;

  // The unknown family is never plain: the row is honest about knowing nothing,
  // and the muted state is what italicises `no detail recorded`.
  const rawState = workflowState(action, stateOf(status, statusBasis));
  const state: LedgerState =
    action.type === "unknown" && rawState === "done" ? "muted" : rawState;

  const object: ObjectPart[] = [...projection.object];
  if (action.type === "unknown") {
    const word = statusWord(status, statusBasis);
    if (word !== null) object.push(part(` · ${word}`));
  }

  const diagnostic = tool.failure?.diagnostic ?? tool.failure?.summary ?? null;
  const rawFailure = failureResult(tool, action);
  const failure =
    action.type === "workflow" && action.run_id == null && rawState === "failed"
      ? rawFailure === null
        ? "did not start"
        : `did not start · ${rawFailure}`
      : rawFailure;
  const timing = tool.timing;
  const startedAt = timing?.started_at ?? null;
  const endedAt = timing?.completed_at ?? null;
  const durationMs =
    timing?.duration_ms ??
    (startedAt !== null && endedAt !== null && endedAt > startedAt ? endedAt - startedAt : null);

  let result: string | null;
  let resultTone: ResultTone;
  if (state === "running") {
    // The component appends the elapsed: `running · 12s`.
    result = "running";
    resultTone = "running";
  } else if (failure !== null) {
    result = failure;
    resultTone = state === "failed" ? "failed" : state === "muted" ? "muted" : "plain";
  } else {
    result = projection.result ?? statusWord(status, statusBasis);
    resultTone =
      state === "failed"
        ? "failed"
        : state === "muted" || projection.resultMuted
          ? "muted"
          : "plain";
  }
  if (result === null && durationMs !== null && durationMs >= 1000) {
    result = spanText(durationMs);
    resultTone = "plain";
  }

  const detail: LedgerDetail =
    projection.detail.kind === "none" && diagnostic !== null
      ? { kind: "diagnostic", diagnostic }
      : projection.detail;

  return {
    key: row.timeline_id,
    rows: [row],
    family: action.type,
    cls: FAMILY_CLASS[action.type],
    glyph: projection.glyph,
    verb: projection.verb,
    object,
    result,
    resultTone,
    state,
    at: endedAt ?? row.at,
    startedAt: startedAt ?? row.at,
    endedAt,
    durationMs,
    detail,
    openByDefault:
      projection.openByDefault ||
      (state === "failed" && diagnostic !== null && diagnostic.includes("\n")),
    folded: false,
    foldCount: 1,
    elided: false,
    childSessionIds: childSessionIdsOf(action),
    diagnostic,
  };
}

/* ── the fold ────────────────────────────────────────────────────────────── */

const SCRIPT_EXTENSIONS = /\.(mjs|cjs|js|ts|tsx|sh|bash|zsh|py|rb|pl|lua)$/i;

/** `.`, `..`, `*` and their kin name nothing; a fold is better off silent. */
const EMPTY_NAMES = new Set(["", ".", "..", "*", "~", "-"]);

function tokenLabel(token: string): string {
  const bare = token.replace(/^["']|["']$/g, "");
  const name = bare.split("/").filter(Boolean).at(-1) ?? bare;
  const trimmed = name.replace(SCRIPT_EXTENSIONS, "");
  return trimmed.length > 24 ? `${trimmed.slice(0, 23)}…` : trimmed;
}

/**
 * A command's name for a fold's list: the program, plus the first argument
 * that is not a flag or a flag's value, each reduced to its own last segment.
 * `pnpm -F field-app test` → `pnpm test`; `cargo clippy --all-targets --
 * -D warnings` → `cargo clippy`; `node scripts/probe-silhouette.mjs --bays 1`
 * → `node probe-silhouette`; `ls` → `ls`. Those are the record's own four
 * drawings, and they are what this reproduces.
 *
 * Flag ARITY is unknowable from the text, so a single-dash flag is assumed to
 * take a value (which is what `-F field-app` needs). When that assumption lands
 * on a token that names nothing — `grep -r x .` reaches `.` — the program
 * stands alone rather than the fold reading `grep .`.
 */
export function commandName(command: string): string {
  const tokens = firstLine(command, 400).split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return "";
  const program = tokenLabel(tokens[0]);
  let argument: string | null = null;
  let index = 1;
  while (index < tokens.length) {
    const token = tokens[index];
    if (token === "--" || token.startsWith("--")) {
      index += 1;
      continue;
    }
    if (token.startsWith("-") && token.length > 1) {
      index += 2;
      continue;
    }
    const label = tokenLabel(token);
    if (EMPTY_NAMES.has(label)) {
      index += 1;
      continue;
    }
    argument = label;
    break;
  }
  return argument === null ? program : `${program} ${argument}`;
}

/**
 * What one folded member is called in the fold's name list: a file by its own
 * name, a search by its query, a page by its host, a command by `commandName`.
 * Only LOOK families ever fold, so every other family answers null.
 */
function memberName(action: ToolAction): string | null {
  switch (action.type) {
    case "shell":
      return commandName(canonicalText(action.command));
    case "file_read": {
      const target = action.targets[0];
      return target === undefined ? null : basename(resourceLabel(target.resource));
    }
    case "file_list":
      return basename(resourceLabel(action.directory));
    case "file_search":
    case "code_lookup":
    case "tool_discovery":
      return action.query;
    case "web_search":
      return action.query;
    case "web_fetch":
      return urlHost(resourceLabel(action.url));
    case "web_navigate":
      return action.url === null ? action.pattern : urlHost(resourceLabel(action.url));
    default:
      return null;
  }
}

function actionOf(row: TimelineEntry): ToolAction | null {
  return row.body.type === "tool_use" ? row.body.tool.action : null;
}

/** The fold's aggregate outcome — one drawing exists (`3 files · 1 truncated`)
 * and the rest follow it: a fold states the same figure its members do, summed. */
function foldResult(family: ToolFamily, rows: readonly TimelineEntry[]): string | null {
  const actions = rows.map(actionOf).filter((action): action is ToolAction => action !== null);
  switch (family) {
    case "file_read": {
      const truncated = actions.filter(
        (action) => action.type === "file_read" && action.outcome?.truncated === true,
      ).length;
      return truncated === 0 ? null : `${truncated} truncated`;
    }
    case "file_list": {
      const entries = sum(
        actions.map((action) => (action.type === "file_list" ? action.outcome?.entry_count : null)),
      );
      return entries === null ? null : `${entries} ${entries === 1 ? "entry" : "entries"}`;
    }
    case "file_search": {
      const matches = sum(
        actions.map((action) =>
          action.type === "file_search" ? action.outcome?.match_count : null,
        ),
      );
      const files = sum(
        actions.map((action) => (action.type === "file_search" ? action.outcome?.file_count : null)),
      );
      if (matches === null) return null;
      if (matches === 0) return "found nothing";
      const pieces = [`${matches} ${matches === 1 ? "match" : "matches"}`];
      if (files !== null && files > 0) pieces.push(`${files} ${files === 1 ? "file" : "files"}`);
      return pieces.join(" · ");
    }
    case "code_lookup": {
      const total = sum(
        actions.map((action) =>
          action.type === "code_lookup" ? action.outcome?.total_count : null,
        ),
      );
      return total === null ? null : `${total} ${total === 1 ? "location" : "locations"}`;
    }
    case "web_search": {
      const counts = actions.map((action) =>
        action.type === "web_search" ? action.outcome?.total_count : null,
      );
      const total = counts.some((count) => count == null) ? null : sum(counts);
      return total === null ? null : `${total} ${total === 1 ? "source" : "sources"}`;
    }
    default:
      return null;
  }
}

function sum(values: readonly (number | null | undefined)[]): number | null {
  const known = values.filter((value): value is number => typeof value === "number");
  return known.length === 0 ? null : known.reduce((total, value) => total + value, 0);
}

/** A fold's detail is its members' details, concatenated in order. */
function foldDetail(family: ToolFamily, lines: readonly LedgerLine[]): LedgerDetail {
  if (family === "shell") {
    const commands: CommandFact[] = lines.flatMap((line) =>
      line.detail.kind === "command" ? [...line.detail.commands] : [],
    );
    return commands.length === 0 ? { kind: "none" } : { kind: "command", commands };
  }
  const paths: PathFact[] = lines.flatMap((line) =>
    line.detail.kind === "paths" ? [...line.detail.paths] : [],
  );
  if (paths.length > 0) return { kind: "paths", paths, total: paths.length };
  if (family === "web_search") {
    const details = lines.flatMap((line) =>
      line.detail.kind === "sources" ? [line.detail] : [],
    );
    return {
      kind: "sources",
      sources: details.flatMap((detail) => [...detail.sources]),
      total: details.some((detail) => detail.total === null)
        ? null
        : sum(details.map((detail) => detail.total)),
    };
  }
  const fetches = lines.flatMap((line) =>
    line.detail.kind === "fetch" ? [line.detail.fetch] : [],
  );
  if (fetches.length > 0) return { kind: "fetches", fetches };
  const locations = lines.flatMap((line) =>
    line.detail.kind === "locations" ? [...line.detail.locations] : [],
  );
  if (locations.length > 0) return { kind: "locations", locations, total: locations.length };
  return { kind: "none" };
}

function foldable(line: LedgerLine): boolean {
  return line.cls === "look" && line.state === "done";
}

function fold(lines: readonly LedgerLine[]): LedgerLine {
  const family = lines[0].family;
  const rows = lines.flatMap((line) => [...line.rows]);
  const names: string[] = [];
  for (const row of rows) {
    const action = actionOf(row);
    const members =
      action?.type === "file_read"
        ? action.targets.map((target) => basename(resourceLabel(target.resource)))
        : [action === null ? null : memberName(action)];
    for (const name of members) {
      if (name !== null && name !== "" && !names.includes(name)) names.push(name);
    }
  }
  // A read line is the file names in both its single and folded forms. The
  // object column already ellipsizes visually, so removing names from the
  // model would make the closed line less useful without saving any space.
  const elided = family !== "file_read" && names.length > FOLD_NAME_LIMIT;
  const [one, many] = FOLD_NOUN[family];
  const noun = rows.length === 1 ? one : many;
  const object: ObjectPart[] =
    family === "file_read" && names.length > 0
      ? [part(names.join(", "))]
      : elided
        ? [part(`… ${rows.length} more ${many}`)]
        : [
            part(`${rows.length} ${noun}`),
            ...(names.length === 0 ? [] : [part(` · ${names.join(", ")}`, "aside")]),
          ];

  const stamps = rows.flatMap(rowStamps);
  const first = stamps.length === 0 ? null : Math.min(...stamps);
  const last = stamps.length === 0 ? null : Math.max(...stamps);
  const durationMs = stampSpan(rows);
  let result = foldResult(family, rows);
  // The elided fold states its span as `→ <end>` in the time slot instead.
  if (result === null && !elided && durationMs !== null && durationMs >= 1000) {
    result = spanText(durationMs);
  }

  return {
    key: lines[0].key,
    rows,
    family,
    cls: lines[0].cls,
    glyph: lines[0].glyph,
    verb: lines[0].verb,
    object,
    result,
    resultTone: "plain",
    state: "done",
    at: elided ? last : first,
    startedAt: first,
    endedAt: last,
    durationMs,
    detail: foldDetail(family, lines),
    openByDefault: false,
    folded: true,
    foldCount: rows.length,
    elided,
    childSessionIds: [],
    diagnostic: null,
  };
}

/**
 * A contiguous run of `tool_use` rows as the lines the ledger draws. Folding
 * happens here and only here — see the rules at the top of this module.
 */
export function ledgerLines(
  rows: readonly TimelineEntry[],
  context: LedgerContext = {},
): LedgerLine[] {
  const singles = rows.map((row) => ledgerLine(row, context));
  const out: LedgerLine[] = [];
  let run: LedgerLine[] = [];
  const flush = () => {
    if (run.length === 0) return;
    out.push(run.length === 1 ? run[0] : fold(run));
    run = [];
  };
  for (const line of singles) {
    if (!foldable(line)) {
      flush();
      out.push(line);
      continue;
    }
    if (run.length > 0 && run[0].family !== line.family) flush();
    run.push(line);
  }
  flush();
  return out;
}

/* ── the run's closing sentence ──────────────────────────────────────────── */

/**
 * One noun of the head's sentence. `names` is non-empty only for `file_change`
 * — the changed files, up to FOLD_NAME_LIMIT, the fold line's own device — and
 * `more` counts the ones past the limit. Two files that share a name keep
 * their directory (`a/index.ts, b/index.ts`), so two edits never read as one.
 * A run with no tool lines says the one thing it holds: `2 thoughts`.
 */
export interface RunNoun {
  readonly family: ToolFamily | "thinking";
  readonly text: string;
  readonly names: readonly string[];
  readonly more: number;
}

/** A member of the run that is not a tool line, as the sentence needs it. */
export type RunOther =
  | { readonly kind: "thinking"; readonly row: TimelineEntry }
  | { readonly kind: "marker"; readonly row: TimelineEntry; readonly failed: boolean }
  | { readonly kind: "summary"; readonly row: TimelineEntry };

export interface RunSummary {
  /** Acts first, then looks, each class by count — `3 edits`, `17 commands`,
   * `4 reads` — counted over ROWS so a fold does not shrink the figure. */
  readonly nouns: readonly RunNoun[];
  /** Lines that did not complete: failed, denied, timed out. Drawn in red at
   * the right beside the span, where a narrow column cannot hide it. */
  readonly failed: number;
  /** Notices inside the run that state a failure: a runtime error, a failed
   * async task. Drawn beside `failed`, in the same red. */
  readonly errors: number;
  /** The whole run's span, from its first known stamp to its last. Raw
   * milliseconds: the record draws a span only once it is at least a second,
   * which is a presentation threshold and stays with the component. */
  readonly spanMs: number | null;
  /** Tool lines the ladder draws. */
  readonly lineCount: number;
  readonly rowCount: number;
  /** Lines plus thoughts, markers and summaries — what decides the fold. */
  readonly memberCount: number;
  /** Reasoning is silent in the sentence unless it is all the run has. */
  readonly thoughtCount: number;
  /** The last line still running — a live head carries it (Q11 A). */
  readonly running: LedgerLine | null;
  /** The run opens closed. The component may still force it open for focus. */
  readonly closedByDefault: boolean;
}

/** The changed files by name. Distinct paths stay distinct: a name shared by
 * two of them carries its parent directory. */
function fileNames(paths: readonly string[]): string[] {
  return paths.map((path, index) => {
    const name = basename(path);
    const twin = paths.some((other, at) => at !== index && basename(other) === name);
    if (!twin) return name;
    const parts = path.split("/").filter(Boolean);
    return parts.length >= 2 ? `${parts[parts.length - 2]}/${name}` : name;
  });
}

export function runSummary(
  lines: readonly LedgerLine[],
  others: readonly RunOther[] = [],
): RunSummary {
  const rows = lines.flatMap((line) => [...line.rows]);
  const order: ToolFamily[] = [];
  const counts = new Map<ToolFamily, number>();
  const paths: string[] = [];
  for (const row of rows) {
    const action = actionOf(row);
    if (action === null) continue;
    if (!counts.has(action.type)) order.push(action.type);
    counts.set(action.type, (counts.get(action.type) ?? 0) + 1);
    if (action.type === "file_change") {
      for (const change of action.changes) {
        const path = resourceLabel(change.path);
        if (path !== "" && !paths.includes(path)) paths.push(path);
      }
    }
  }
  const names = fileNames(paths);
  // Acts lead: the work product is what a reader wants first, and what
  // survives when a narrow column ellipsizes the tail. Within a class the
  // larger count comes first; ties keep first occurrence.
  const byClass = (cls: LedgerClass): ToolFamily[] =>
    order
      .filter((family) => FAMILY_CLASS[family] === cls)
      .sort(
        (a, b) =>
          (counts.get(b) ?? 0) - (counts.get(a) ?? 0) || order.indexOf(a) - order.indexOf(b),
      );
  const nouns: RunNoun[] = [...byClass("act"), ...byClass("look")].map((family) => ({
    family,
    text: familyNoun(family, counts.get(family) ?? 0),
    names: family === "file_change" ? names.slice(0, FOLD_NAME_LIMIT) : [],
    more: family === "file_change" ? Math.max(0, names.length - FOLD_NAME_LIMIT) : 0,
  }));
  const thoughtCount = others.filter((other) => other.kind === "thinking").length;
  if (nouns.length === 0 && thoughtCount > 0) {
    nouns.push({
      family: "thinking",
      text: `${thoughtCount} ${thoughtCount === 1 ? "thought" : "thoughts"}`,
      names: [],
      more: 0,
    });
  }
  const allRows = [...rows, ...others.map((other) => other.row)];
  const memberCount = lines.length + others.length;
  return {
    nouns,
    failed: lines.filter((line) => line.state === "failed").length,
    errors: others.filter((other) => other.kind === "marker" && other.failed).length,
    spanMs: stampSpan(allRows),
    lineCount: lines.length,
    rowCount: allRows.length,
    memberCount,
    thoughtCount,
    running: [...lines].reverse().find((line) => line.state === "running") ?? null,
    // Notices and summaries alone have no sentence to close to (R-2).
    // Questions addressed to the reader are speech and stay visible (R-1).
    closedByDefault:
      memberCount >= RUN_FOLD_THRESHOLD &&
      (lines.length > 0 || thoughtCount > 0) &&
      !lines.some((line) => line.family === "question"),
  };
}
