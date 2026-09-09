// §04 — the vocabulary, family by family. Twenty-eight closed families, one
// sentence each: `verb · object · result`, plus the glyph and what the line
// opens into.
//
// This module knows nothing about status, folding or time. It projects the
// ACTION and only the action; `ledger.ts` wraps it in the evidence (T-5's hue,
// the stamps, the fold). Splitting them is what keeps the failure rules in one
// place instead of twenty-eight.
import type {
  ParticipantColor,
  ResourceRef,
  TaskSnapshotItem,
  ToolAction,
} from "@rosetta/protocol";
import type {
  CanonicalDocument,
  CollaboratorRef,
  DisplayField,
  DisplayScalar,
  DisplayValue,
  FileDelta,
  FindingItem,
  FindingSeverity,
  NativeToolRef,
  SourceLocation,
  SourceRange,
  UserAnswer,
  UserQuestion,
} from "./protocol.ts";
import { clockShort, duration } from "../format.ts";
import { webAddress } from "./web-address.ts";
import type { LedgerContext, LedgerGlyph, ObjectPart, ToolFamily } from "./vocabulary.ts";
import {
  FAMILY_GLYPH,
  canonicalText,
  firstLine,
  part,
  pathParts,
  resourceDisplay,
  resourceLabel,
  splitPath,
} from "./vocabulary.ts";

/* ── detail payloads ─────────────────────────────────────────────────────── */

/** One pressable path in a detail — pre-split so the list draws like a line. */
export interface PathFact {
  readonly label: string;
  readonly dir: string;
  readonly name: string;
  readonly resource: ResourceRef;
  /** A read target's own source range, drawn beside that file in its detail. */
  readonly range?: string;
  /** Present only when this path is an exact file target of a canonical row.
   * It is an address into that row, not a renderer-supplied filesystem path,
   * so the daemon can reopen the recorded file without widening its read
   * authority. */
  readonly read?: {
    readonly sessionId: string;
    readonly timelineId: string;
    readonly targetIndex: number;
  };
}

/** A web result. The protocol keeps url and title only; the HOST is derived,
 * and it is the key the renderer caches `https://<host>/favicon.ico` under. */
export interface SourceFact {
  readonly url: string;
  readonly title: string | null;
  readonly host: string | null;
}

/** A fetch's honest triple: final url, title, status — never the body. */
export interface FetchFact {
  readonly url: string | null;
  readonly title: string | null;
  readonly status: number | null;
  readonly host: string | null;
}

/** One command inside a `command` or `code` detail. A fold carries several. */
export interface CommandFact {
  readonly command: string;
  readonly cwd: string | null;
  /** Whether this command ran at the session's settled root. */
  readonly cwdIsSessionRoot: boolean;
  readonly language: string | null;
  readonly background: boolean;
  readonly sourceTruncated: boolean;
  readonly sessionId: string | null;
  readonly timelineId: string | null;
}

/** The command row is a glanceable label, not a second source viewer. Keep
 * its omission marker inside the 42-character contract. `Array.from` counts
 * Unicode code points, matching Rust's projection boundary. */
export const COMMAND_PREVIEW_CHARACTER_LIMIT = 42;
const COMMAND_PREVIEW_OMISSION = "...";
const COMMAND_PREVIEW_CONTENT_LIMIT =
  COMMAND_PREVIEW_CHARACTER_LIMIT - COMMAND_PREVIEW_OMISSION.length;

export function commandPreviewText(text: string, sourceTruncated: boolean): string {
  const lines = text.split(/\r?\n/);
  const line = lines.find((candidate) => candidate.trim() !== "") ?? "";
  const characters = Array.from(line);
  const omitted =
    sourceTruncated || lines.length > 1 || characters.length > COMMAND_PREVIEW_CHARACTER_LIMIT;
  if (!omitted) return characters.join("");
  return `${characters.slice(0, COMMAND_PREVIEW_CONTENT_LIMIT).join("")}${COMMAND_PREVIEW_OMISSION}`;
}

export interface CommandDirectoryContext {
  readonly label: string;
  readonly title: string;
}

/** Directory context gets one compact badge on the detail frame. A mixed fold
 * is named honestly rather than borrowing the first command's cwd for every
 * member. */
export function commandDirectoryContext(
  commands: readonly CommandFact[],
): CommandDirectoryContext | null {
  const known = commands.filter(
    (command): command is CommandFact & { readonly cwd: string } => command.cwd !== null,
  );
  if (known.length === 0) return null;

  const directories = [...new Set(known.map((command) => command.cwd))];
  if (directories.length === 1) {
    return {
      label: directories[0],
      title: `Working directory: ${directories[0]}`,
    };
  }
  return {
    label: `${directories.length} folders`,
    title: `Working directories: ${directories.join(", ")}`,
  };
}

function isSessionCwd(resource: ResourceRef, context: LedgerContext): boolean {
  if (context.cwd === null || context.cwd === undefined) return false;
  if (resource.type !== "file" && resource.type !== "directory") return false;
  const source = resourceLabel(resource).replace(/\/+$/, "");
  const session = context.cwd.replace(/\/+$/, "");
  return source === session;
}

/** `+18 −4` across however many files the one row changed. */
export interface Diffstat {
  readonly added: number | null;
  readonly removed: number | null;
  readonly fileCount: number;
}

/** Everything the delegate line knows before the child session is resolved. */
export interface DelegateFact {
  readonly childSessionId: string | null;
  readonly agentKind: string | null;
  readonly name: string | null;
  readonly model: string | null;
  /** Named only when native evidence proves persistent team membership. */
  readonly team: string | null;
  readonly color: ParticipantColor | null;
  readonly resumeId: string | null;
  readonly objective: string;
  readonly background: boolean;
  /** The child was never indexed; the record says draw its summary as prose. */
  readonly orphanSummary: string | null;
}

export type WorkflowAction = Extract<ToolAction, { readonly type: "workflow" }>;

/** The durable Workflow document plus the exact file addresses exposed by
 * the row. The first path, when present, is the workflow script; remaining
 * paths are result resources discovered in the sidecar/callback. */
export interface WorkflowFact {
  readonly action: WorkflowAction;
  readonly objective: string | null;
  readonly script: PathFact | null;
  readonly resources: readonly PathFact[];
}

/** The one task mutation this historical board is explaining. */
export type TaskChange =
  | "added"
  | "started"
  | "done"
  | "reopened"
  | "failed"
  | "cancelled"
  | "updated";

/** The open task row is a status glimpse first; the reader can disclose the
 * complete recorded board without turning every task event into a wall. */
export const TASK_PREVIEW_LIMIT = 3;

/** Pin the task this event changed to the bottom, where the eye lands after
 * reading the board. The compact form keeps its two nearest preceding peers
 * above it (filling from following peers only at the start of a board); the
 * full form retains every other recorded task in its original relative order. */
export function taskDisplayRows(
  tasks: readonly TaskSnapshotItem[],
  changedTaskId: string | null,
  full: boolean,
): TaskSnapshotItem[] {
  const currentAt =
    changedTaskId === null ? -1 : tasks.findIndex((task) => task.task_id === changedTaskId);
  const ordered =
    currentAt < 0
      ? [...tasks]
      : [...tasks.slice(0, currentAt), ...tasks.slice(currentAt + 1), tasks[currentAt]];
  if (full || ordered.length <= TASK_PREVIEW_LIMIT) return ordered;
  if (currentAt < 0) return ordered.slice(-TASK_PREVIEW_LIMIT);

  const peerLimit = TASK_PREVIEW_LIMIT - 1;
  const before = tasks.slice(Math.max(0, currentAt - peerLimit), currentAt);
  const after = tasks.slice(currentAt + 1, currentAt + 1 + (peerLimit - before.length));
  return [...before, ...after, tasks[currentAt]];
}

/**
 * What a ledger line opens into (§04's last column). `none` means the line has
 * nothing honest to show. A read keeps no content in SQLite but does retain
 * its target addresses; an `unknown` row keeps nothing at all.
 */
export type LedgerDetail =
  | { readonly kind: "none" }
  | {
      readonly kind: "paths";
      readonly paths: readonly PathFact[];
      readonly total: number;
      readonly description?: string;
    }
  | {
      readonly kind: "locations";
      readonly locations: readonly SourceLocation[];
      readonly total: number;
    }
  | { readonly kind: "sources"; readonly sources: readonly SourceFact[]; readonly total: number | null }
  | { readonly kind: "fetch"; readonly fetch: FetchFact }
  | { readonly kind: "fetches"; readonly fetches: readonly FetchFact[] }
  | { readonly kind: "diff"; readonly deltas: readonly FileDelta[]; readonly diffstat: Diffstat }
  | { readonly kind: "command"; readonly commands: readonly CommandFact[] }
  | { readonly kind: "code"; readonly commands: readonly CommandFact[] }
  | { readonly kind: "names"; readonly names: readonly string[]; readonly total: number }
  | {
      readonly kind: "tasks";
      readonly tasks: readonly TaskSnapshotItem[];
      readonly total: number;
      readonly changedTaskId: string | null;
      readonly change: TaskChange | null;
    }
  | { readonly kind: "text"; readonly text: string }
  | {
      readonly kind: "questions";
      readonly questions: readonly UserQuestion[];
      readonly answers: readonly UserAnswer[];
    }
  | { readonly kind: "findings"; readonly findings: readonly FindingItem[]; readonly total: number }
  | { readonly kind: "document"; readonly document: CanonicalDocument }
  | {
      readonly kind: "fields";
      readonly fields: readonly DisplayField[];
      readonly resources: readonly PathFact[];
    }
  | { readonly kind: "delegate"; readonly delegate: DelegateFact }
  | { readonly kind: "workflow"; readonly workflow: WorkflowFact }
  | {
      readonly kind: "participants";
      readonly participants: readonly CollaboratorRef[];
      readonly total: number;
      readonly body: string | null;
    }
  | { readonly kind: "diagnostic"; readonly diagnostic: string };

export type DetailKind = LedgerDetail["kind"];

const NO_DETAIL: LedgerDetail = { kind: "none" };

/* ── the projection ──────────────────────────────────────────────────────── */

export interface FamilyProjection {
  readonly family: ToolFamily;
  readonly glyph: LedgerGlyph;
  readonly verb: string;
  readonly object: readonly ObjectPart[];
  /** The family's own outcome sentence, before status and failure have their
   * say. Null when the family has no outcome to state. */
  readonly result: string | null;
  /** `trace unavailable` and its kin: an outcome that IS the absence of one. */
  readonly resultMuted: boolean;
  readonly detail: LedgerDetail;
  /** §04: a question, a finding and a structured document open by default —
   * a question is conversation, and a finding is the point of the run. */
  readonly openByDefault: boolean;
}

/* ── small shared shapes ─────────────────────────────────────────────────── */

function plural(count: number, one: string, many = `${one}s`): string {
  return `${count} ${count === 1 ? one : many}`;
}

function pathFact(
  resource: ResourceRef,
  context: LedgerContext,
  read?: PathFact["read"],
  range?: string,
): PathFact {
  const label = resourceDisplay(resource, context);
  const { dir, name } = splitPath(label);
  return {
    label,
    dir,
    name,
    resource,
    ...(range === undefined ? {} : { range }),
    ...(read === undefined ? {} : { read }),
  };
}

function pathFacts(
  resources: readonly ResourceRef[],
  context: LedgerContext,
  source: Omit<NonNullable<PathFact["read"]>, "targetIndex"> | null = null,
): PathFact[] {
  return resources.map((resource, targetIndex) =>
    pathFact(
      resource,
      context,
      source !== null && resource.type === "file" ? { ...source, targetIndex } : undefined,
    ),
  );
}

function sameResource(left: ResourceRef, right: ResourceRef): boolean {
  return left.type === right.type && resourceLabel(left) === resourceLabel(right);
}

/** Keep the renderer and `/file` target indices deterministic: workflow
 * script first, then each distinct result resource in native order. */
function workflowPathFacts(
  action: WorkflowAction,
  context: LedgerContext,
  source: { readonly sessionId: string; readonly timelineId: string } | null,
): { readonly script: PathFact | null; readonly resources: readonly PathFact[] } {
  const scriptResource = action.run?.script_path ?? action.path;
  const resources: ResourceRef[] = [];
  if (scriptResource !== null) resources.push(scriptResource);
  for (const resource of action.run?.resources ?? []) {
    if (!resources.some((existing) => sameResource(existing, resource))) resources.push(resource);
  }
  const facts = pathFacts(resources, context, source);
  return {
    script: scriptResource === null ? null : (facts[0] ?? null),
    resources: scriptResource === null ? facts : facts.slice(1),
  };
}

function workflowPhaseAside(action: WorkflowAction): string | null {
  if ((action.attempt ?? 1) > 1 || action.resume_id !== null) {
    return `attempt ${action.attempt ?? 2}, same run`;
  }
  const run = action.run ?? null;
  if (run === null || run.phases.length === 0) {
    return action.objective === null || action.objective === undefined
      ? null
      : firstLine(canonicalText(action.objective), 68);
  }
  const active = run.workers.find((worker) => worker.state === "progress");
  const observed = active?.phase_index ?? Math.max(
    0,
    ...run.workers
      .filter((worker) => worker.state !== "start" && worker.phase_index !== null)
      .map((worker) => worker.phase_index ?? 0),
  );
  if (observed > 0 && run.status !== "completed") {
    return `phase ${observed} of ${run.phases.length}`;
  }
  const objective = action.objective ?? run.summary;
  return objective === null ? null : firstLine(canonicalText(objective), 68);
}

function workflowResult(action: WorkflowAction): string | null {
  const run = action.run ?? null;
  if (run === null) return action.background ? "in background" : null;
  const total = run.workers.length;
  const done = run.workers.filter((worker) => worker.state === "done").length;
  const status = run.status === "unknown" ? "status unknown" : run.status;
  return total === 0 ? status : `${status} · ${done}/${total}`;
}

/** `L1–180` · `p3–4`, carried by the exact file target it describes. */
function rangeText(range: SourceRange): string {
  const mark = range.unit === "line" ? "L" : "p";
  const end = range.count === null ? null : range.start + range.count - 1;
  return end === null || end <= range.start
    ? `${mark}${range.start}`
    : `${mark}${range.start}–${end}`;
}

function joined(pieces: readonly (string | null)[]): string | null {
  const kept = pieces.filter((piece): piece is string => piece !== null && piece !== "");
  return kept.length === 0 ? null : kept.join(" · ");
}

function displayScalarText(value: DisplayScalar): string {
  switch (value.type) {
    case "text":
    case "number":
      return value.value;
    case "boolean":
      return value.value ? "yes" : "no";
  }
}

function displayValueText(value: DisplayValue): string {
  switch (value.type) {
    case "text":
    case "number":
      return value.value;
    case "boolean":
      return value.value ? "yes" : "no";
    case "resource":
      return resourceLabel(value.value);
    case "list":
      return value.value.map(displayScalarText).join(", ");
  }
}

const SEARCH_QUALIFIER: Readonly<Record<string, string>> = {
  name: "for files",
  glob: "for files",
  content: "in files",
  regex: "in files",
  mixed: "in files",
};

const PROCESS_VERB: Readonly<Record<string, string>> = {
  wait: "Waited for",
  input: "Sent input to",
  stop: "Stopped",
  inspect: "Inspected",
};

const EXEC_CHILD_LABEL: Readonly<
  Record<string, { readonly verb: string; readonly noun: string; readonly many?: string }>
> = {
  exec_command: { verb: "Ran", noun: "command" },
  write_stdin: { verb: "Sent input to", noun: "process", many: "processes" },
  apply_patch: { verb: "Applied", noun: "patch", many: "patches" },
  mcp__node_repl__js: { verb: "Ran", noun: "script" },
  update_plan: { verb: "Updated", noun: "plan" },
  web__run: { verb: "Browsed", noun: "web request" },
  view_image: { verb: "Viewed", noun: "image" },
  get_goal: { verb: "Checked", noun: "goal" },
};

const CHANGE_VERB: Readonly<Record<string, string>> = {
  create: "Created",
  update: "Edited",
  delete: "Deleted",
  move: "Moved",
};

const SCHEDULE_VERB: Readonly<Record<string, string>> = {
  create: "Scheduled a wake-up",
  delete: "Cancelled",
  wake: "Woke",
  list: "Listed schedules",
};

const ARTIFACT_VERB: Readonly<Record<string, string>> = {
  create: "Made an artifact",
  edit: "Edited an artifact",
  send: "Sent an artifact",
};

const COLLABORATE_VERB: Readonly<Record<string, string>> = {
  send: "Sent to",
  request_stop: "Asked to stop",
  respond_stop: "Responded to stop",
  create: "Created",
  delete: "Deleted",
  list: "Listed",
};

const SEVERITY_ORDER = ["info", "low", "medium", "high", "critical"] as const;

function taskChange(operation: string, status: string | null): TaskChange | null {
  if (operation === "create") return "added";
  if (operation !== "update") return null;
  switch (status) {
    case "in_progress":
      return "started";
    case "completed":
      return "done";
    case "pending":
      return "reopened";
    case "failed":
      return "failed";
    case "cancelled":
      return "cancelled";
    default:
      return "updated";
  }
}

function taskVerb(operation: string, status: string | null): string {
  if (operation === "create") return "Added";
  if (operation === "list") return "Checked";
  if (operation === "get" || operation === "output") return "Checked";
  if (operation === "stop") return "Stopped";
  switch (status) {
    case "in_progress":
      return "Started";
    case "completed":
      return "Completed";
    case "pending":
      return "Reopened";
    case "failed":
      return "Marked failed";
    case "cancelled":
      return "Cancelled";
    default:
      return "Updated";
  }
}

/**
 * The §04 table as one function. `native` is only read by the `unknown` family,
 * whose whole point is to name the tool the projector could not read.
 */
export function projectAction(
  action: ToolAction,
  native: NativeToolRef,
  context: LedgerContext = {},
  source: { readonly sessionId: string; readonly timelineId: string } | null = null,
): FamilyProjection {
  const base = {
    family: action.type,
    glyph: FAMILY_GLYPH[action.type],
    resultMuted: false,
    openByDefault: false,
  } as const;

  switch (action.type) {
    /* ── Looking ─────────────────────────────────────────────────────────── */

    case "file_read": {
      const facts = action.targets.map((target, targetIndex) =>
        pathFact(
          target.resource,
          context,
          source !== null && target.resource.type === "file"
            ? { ...source, targetIndex }
            : undefined,
          target.range === null ? undefined : rangeText(target.range),
        ),
      );
      const media = action.targets.find((target) => target.media_kind !== null)?.media_kind ?? null;
      const object =
        facts.length > 0
          ? [part(facts.map((fact) => fact.name || fact.label).join(", "))]
          : [part("no target recorded", "aside")];
      return {
        ...base,
        verb: "Read",
        object,
        result: joined([
          action.outcome?.truncated ? "truncated" : null,
          media,
        ]),
        // Read content stays out of the projection. The exact target address
        // lets an explicit click ask for the live file through the viewer.
        detail:
          facts.length > 0 ? { kind: "paths", paths: facts, total: facts.length } : NO_DETAIL,
      };
    }

    case "file_list": {
      const label = resourceDisplay(action.directory, context);
      return {
        ...base,
        verb: "Listed",
        object: pathParts(label),
        result: action.outcome === null ? null : plural(action.outcome.entry_count, "entry", "entries"),
        detail: NO_DETAIL,
      };
    }

    case "file_search": {
      const outcome = action.outcome;
      const facts = pathFacts(outcome?.paths ?? [], context);
      return {
        ...base,
        verb: "Searched",
        object: [part(action.query, "query"), part(` ${SEARCH_QUALIFIER[action.mode]}`, "aside")],
        result:
          outcome === null
            ? null
            : outcome.match_count === 0
              ? // §04's own word when the source is honest about zero. When the
                // source instead REPORTS zero as a failure (Grok does), the
                // failure summary wins in `ledger.ts` and the line goes red.
                "found nothing"
              : joined([
                  plural(outcome.match_count, "match", "matches"),
                  outcome.file_count > 0 ? plural(outcome.file_count, "file") : null,
                ]),
        detail:
          facts.length === 0
            ? NO_DETAIL
            : { kind: "paths", paths: facts, total: outcome?.total_paths ?? facts.length },
      };
    }

    case "code_lookup": {
      const outcome = action.outcome;
      return {
        ...base,
        verb: "Looked up",
        object: [part(action.query, "query"), part(` ${action.operation}`, "aside")],
        result: outcome === null ? null : plural(outcome.total_count, "location"),
        detail:
          outcome === null || outcome.locations.length === 0
            ? NO_DETAIL
            : { kind: "locations", locations: outcome.locations, total: outcome.total_count },
      };
    }

    case "web_search": {
      const outcome = action.outcome;
      const sources: SourceFact[] = (outcome?.sources ?? []).map((source) => ({
        url: source.url,
        title: source.title?.trim() || null,
        host: webAddress(source.url)?.host ?? null,
      }));
      const query = action.query ?? action.allowed_domains.join(", ");
      return {
        ...base,
        verb: "Searched the web",
        object: query ? [part(query, "query")] : [part("no query recorded", "aside")],
        result: outcome === null ? null : plural(outcome.total_count, "source"),
        detail: { kind: "sources", sources, total: outcome?.total_count ?? null },
      };
    }

    case "web_fetch": {
      const label = resourceLabel(action.url);
      const outcome = action.outcome;
      const status = outcome?.http_status ?? null;
      const finalUrl = outcome?.final_url ?? (action.url.type === "url" ? action.url.url : null);
      return {
        ...base,
        verb: "Fetched",
        object: pathParts(label),
        result: status === null ? null : `HTTP ${status}`,
        detail: {
          kind: "fetch",
          fetch: {
            url: finalUrl,
            title: outcome?.title ?? null,
            status: outcome?.http_status ?? null,
            host: webAddress(finalUrl)?.host ?? null,
          },
        },
      };
    }

    case "web_navigate": {
      const label = action.url ? resourceLabel(action.url) : (action.pattern ?? "");
      const outcome = action.outcome;
      const status = outcome?.http_status ?? null;
      const finalUrl = outcome?.final_url ?? (action.url?.type === "url" ? action.url.url : null);
      return {
        ...base,
        verb: action.operation === "find_in_page" ? "Found in page" : "Opened",
        object: action.operation === "find_in_page" && action.pattern
          ? [
              part(action.pattern, "query"),
              ...(action.url ? [part(" in ", "aside"), ...pathParts(label)] : []),
            ]
          : label ? pathParts(label) : [part("no target recorded", "aside")],
        result: status === null ? null : `HTTP ${status}`,
        detail: {
          kind: "fetch",
          fetch: {
            url: finalUrl,
            title: outcome?.title ?? null,
            status: outcome?.http_status ?? null,
            host: webAddress(finalUrl)?.host ?? null,
          },
        },
      };
    }

    case "tool_discovery":
      return {
        ...base,
        verb: "Searched tools",
        object: [part(action.query, "query")],
        result: `${action.matches.length} of ${action.total_count}`,
        detail:
          action.matches.length === 0
            ? NO_DETAIL
            : { kind: "names", names: action.matches, total: action.total_count },
      };

    /* ── Acts ────────────────────────────────────────────────────────────── */

    case "file_change": {
      const changes = action.changes;
      const operations = new Set(changes.map((change) => change.operation));
      const only = operations.size === 1 ? [...operations][0] : null;
      // `Changed` is the honest verb when the source could not prove which
      // operation ran, or when one row mixed several.
      const verb = only === null || only === undefined ? "Changed" : CHANGE_VERB[only];
      const added = sumOrNull(changes.map((change) => change.lines_added));
      const removed = sumOrNull(changes.map((change) => change.lines_removed));
      const stat = diffstatText(added, removed);
      const hasDiff = changes.some((change) => change.diff !== null);
      const first = changes[0] ?? null;
      const object: ObjectPart[] =
        changes.length > 1
          ? [part(plural(changes.length, "file"))]
          : first === null
            ? [part("no path recorded", "aside")]
            : [
                ...pathParts(resourceDisplay(first.path, context)),
                ...(first.destination === null
                  ? []
                  : [part(` → ${resourceDisplay(first.destination, context)}`, "aside")]),
              ];
      return {
        ...base,
        verb,
        object,
        // Absence is stated, never faked with a zero (§06).
        result: stat ?? (hasDiff ? null : "no diff recorded"),
        detail: {
          kind: "diff",
          deltas: changes,
          diffstat: { added, removed, fileCount: changes.length },
        },
      };
    }

    case "shell": {
      const command = firstLine(canonicalText(action.command));
      return {
        ...base,
        verb: "Ran",
        object: [
          part(command),
          ...(action.background ? [part(" in background", "aside")] : []),
        ],
        result: null,
        detail: {
          kind: "command",
          commands: [
            {
              command: canonicalText(action.command),
              cwd: action.cwd === null ? null : resourceDisplay(action.cwd, context),
              cwdIsSessionRoot: action.cwd === null ? false : isSessionCwd(action.cwd, context),
              language: null,
              background: action.background,
              sourceTruncated: action.source_truncated === true,
              sessionId: source?.sessionId ?? null,
              timelineId: source?.timelineId ?? null,
            },
          ],
        },
      };
    }

    case "code_execute": {
      const code = firstLine(canonicalText(action.code));
      return {
        ...base,
        verb: "Ran",
        object: [
          ...(action.language === null ? [] : [part(`${action.language} `, "aside")]),
          part(code),
        ],
        result: null,
        detail: {
          kind: "code",
          commands: [
            {
              command: canonicalText(action.code),
              cwd: action.cwd === null ? null : resourceDisplay(action.cwd, context),
              cwdIsSessionRoot: action.cwd === null ? false : isSessionCwd(action.cwd, context),
              language: action.language,
              background: false,
              sourceTruncated: action.source_truncated === true,
              sessionId: source?.sessionId ?? null,
              timelineId: source?.timelineId ?? null,
            },
          ],
        },
      };
    }

    case "process": {
      const input = action.input === null ? null : canonicalText(action.input);
      return {
        ...base,
        verb: PROCESS_VERB[action.operation],
        object: [part(action.process_id)],
        result: action.outcome?.state ?? null,
        detail: input === null ? NO_DETAIL : { kind: "text", text: input },
      };
    }

    case "orchestration": {
      const unavailable = action.trace === "unavailable";
      const childLabels = action.executed_children.map((child) => EXEC_CHILD_LABEL[child] ?? null);
      const firstChild = childLabels[0] ?? null;
      const oneKnownKind =
        firstChild !== null &&
        childLabels.every(
          (child) =>
            child !== null &&
            child.verb === firstChild.verb &&
            child.noun === firstChild.noun &&
            child.many === firstChild.many,
        );
      const childCount =
        action.executed_child_count ??
        (action.trace === "complete" ? action.executed_children.length : null);
      if (oneKnownKind) {
        return {
          ...base,
          verb: firstChild.verb,
          object: [
            part(
              childCount === null
                ? (firstChild.many ?? `${firstChild.noun}s`)
                : plural(childCount, firstChild.noun, firstChild.many),
            ),
          ],
          result: null,
          detail:
            action.executed_children.length === 0
              ? NO_DETAIL
              : {
                  kind: "names",
                  names: action.executed_children,
                  total: action.executed_child_count ?? action.executed_children.length,
                },
        };
      }
      if (action.runtime === "functions.exec") {
        const kinds = [
          ...new Set(
            action.executed_children.map((child, index) => {
              const label = childLabels[index];
              return label?.noun ?? child.replaceAll("_", " ");
            }),
          ),
        ];
        return {
          ...base,
          verb: "Ran",
          object: [part("tool batch")],
          result: kinds.length === 0 ? "trace unavailable" : kinds.slice(0, 3).join(" · "),
          resultMuted: kinds.length === 0,
          detail:
            action.executed_children.length === 0
              ? NO_DETAIL
              : {
                  kind: "names",
                  names: action.executed_children,
                  total: action.executed_child_count ?? action.executed_children.length,
                },
        };
      }
      return {
        ...base,
        verb: "Executed",
        object: [part(action.runtime ?? "children")],
        result:
          action.executed_child_count === null
            ? unavailable
              ? "trace unavailable"
              : null
            : plural(action.executed_child_count, "child", "children"),
        resultMuted: unavailable && action.executed_child_count === null,
        detail:
          action.executed_children.length === 0
            ? NO_DETAIL
            : {
                kind: "names",
                names: action.executed_children,
                total: action.executed_child_count ?? action.executed_children.length,
              },
      };
    }

    case "delegate": {
      const objective = firstLine(canonicalText(action.objective));
      const label = action.name ?? action.agent_kind;
      return {
        ...base,
        verb: action.resume_id === null ? "Delegated" : "Resumed",
        object: [
          ...(label === null ? [] : [part(label)]),
          part(label === null ? objective : ` · ${objective}`, "aside"),
        ],
        // `14 messages · 48s` needs the CHILD, which only the component can
        // resolve; the projection states what it can prove and no more.
        result: action.background ? "in background" : null,
        detail: {
          kind: "delegate",
          delegate: {
            childSessionId: action.child_session_id,
            agentKind: action.agent_kind,
            name: action.name,
            model: action.model,
            team: action.team,
            color: action.color,
            resumeId: action.resume_id,
            objective: canonicalText(action.objective),
            background: action.background,
            orphanSummary:
              action.orphan_result_summary === null
                ? null
                : canonicalText(action.orphan_result_summary),
          },
        },
      };
    }

    case "collaborate": {
      const target = action.recipient ?? action.team;
      const summary = action.summary ?? (action.body ? firstLine(canonicalText(action.body)) : null);
      return {
        ...base,
        verb: COLLABORATE_VERB[action.operation],
        object: [
          ...(target === null ? [] : [part(target)]),
          ...(summary === null
            ? []
            : [part(target === null ? summary : ` · ${summary}`, "aside")]),
        ],
        result:
          action.outcome === null ? null : plural(action.outcome.total_count, "participant"),
        detail:
          action.outcome === null
            ? action.body === null
              ? NO_DETAIL
              : { kind: "text", text: canonicalText(action.body) }
            : {
                kind: "participants",
                participants: action.outcome.participants,
                total: action.outcome.total_count,
                body: action.body === null ? null : canonicalText(action.body),
              },
      };
    }

    case "question": {
      const prompt = action.questions[0] ? firstLine(canonicalText(action.questions[0].prompt)) : "";
      return {
        ...base,
        verb: "Asked",
        object: [part(prompt || "a question", "aside")],
        result: action.answers.length > 0 ? "answered" : null,
        // Conversation, so it opens by default.
        openByDefault: true,
        detail: { kind: "questions", questions: action.questions, answers: action.answers },
      };
    }

    case "skill":
      return {
        ...base,
        verb: "Used skill",
        object: [part(action.name)],
        result: null,
        detail:
          action.arguments === null
            ? NO_DETAIL
            : { kind: "text", text: canonicalText(action.arguments) },
      };

    case "workflow": {
      const label =
        action.name ?? (action.path === null ? null : resourceDisplay(action.path, context));
      const run = action.run ?? null;
      const objective = action.objective ?? run?.summary ?? null;
      const paths = workflowPathFacts(action, context, source);
      const aside = workflowPhaseAside(action);
      return {
        ...base,
        // A hydrated workflow is itself a small document: its attempts,
        // phases, workers, and result are the useful message. Keeping that
        // whole document behind the generic ledger disclosure made the real
        // UI disagree with the open workflow state in the design, and left
        // the nested worker-thread disclosure practically undiscoverable.
        // Readers can still close it, but a fresh transcript must expose its
        // structure without requiring a first, unlabeled click.
        openByDefault: run !== null,
        glyph:
          run !== null && ["killed", "cancelled", "stopped"].includes(run.status)
            ? "clock"
            : base.glyph,
        verb: (action.attempt ?? 1) > 1 || action.resume_id !== null
          ? "Resumed workflow"
          : "Ran workflow",
        object: [
          ...(label === null ? [part("dynamic workflow", "aside")] : pathParts(label)),
          ...(aside === null ? [] : [part(` · ${aside}`, "aside")]),
        ],
        result: workflowResult(action),
        detail: {
          kind: "workflow",
          workflow: {
            action,
            objective: objective === null ? null : canonicalText(objective),
            script: paths.script,
            resources: paths.resources,
          },
        },
      };
    }

    case "schedule":
      return {
        ...base,
        verb: SCHEDULE_VERB[action.operation],
        object: [part(action.reason ?? action.schedule_id ?? "a wake-up", "aside")],
        result:
          action.delay_ms !== null
            ? `in ${duration(0, action.delay_ms)}`
            : action.due_at !== null
              ? `at ${clockShort(action.due_at)}`
              : null,
        detail:
          action.objective === null
            ? NO_DETAIL
            : { kind: "text", text: canonicalText(action.objective) },
      };

    case "artifact": {
      const media = action.media;
      const resources = pathFacts(action.resources, context, source);
      return {
        ...base,
        verb: ARTIFACT_VERB[action.operation],
        object: [
          part(action.name ?? action.artifact_kind),
          ...(action.name === null ? [] : [part(` · ${action.artifact_kind}`, "aside")]),
        ],
        result:
          media !== null && media.width !== null && media.height !== null
            ? `${media.width}×${media.height}`
            : action.resources.length > 0
              ? plural(action.resources.length, "file")
              : null,
        detail:
          resources.length > 0
            ? {
                kind: "paths",
                paths: resources,
                total: resources.length,
                ...(action.description === null
                  ? {}
                  : { description: canonicalText(action.description) }),
              }
            : action.description === null
              ? NO_DETAIL
              : { kind: "text", text: canonicalText(action.description) },
      };
    }

    case "notification":
      return {
        ...base,
        verb: "Notified",
        object: [
          part(action.channel ?? "a channel"),
          ...(action.destination === null ? [] : [part(` → ${action.destination}`, "aside")]),
        ],
        result: null,
        detail: { kind: "text", text: canonicalText(action.summary) },
      };

    case "mode":
      return {
        ...base,
        verb: action.operation === "enter" ? "Entered" : "Left",
        object: [
          part(`${action.mode} mode`),
          ...(action.workspace === null
            ? []
            : [part(` · ${resourceDisplay(action.workspace, context)}`, "aside")]),
        ],
        result: null,
        // The matching `mode_changed` notice is the marker; this line is not
        // repeated there and there is nothing more to open.
        detail: NO_DETAIL,
      };

    /* ── Structure ───────────────────────────────────────────────────────── */

    case "task": {
      const snapshot = action.snapshot;
      const changed =
        snapshot === null || action.task_id === null
          ? null
          : (snapshot.tasks.find((task) => task.task_id === action.task_id) ?? null);
      const subject =
        action.subject ??
        changed?.subject ??
        (action.description === null ? null : firstLine(canonicalText(action.description)));
      const isList = action.operation === "list";
      const completed = snapshot?.tasks.filter((task) => task.status === "completed").length ?? 0;
      return {
        ...base,
        verb: taskVerb(action.operation, action.status),
        object: isList
          ? [part("task list", "aside")]
          : [
              part(
                subject ?? (action.task_id === null ? "task" : `task #${action.task_id}`),
                subject === null ? "aside" : "plain",
              ),
            ],
        result:
          snapshot === null
            ? action.status === null
              ? null
              : action.status.replaceAll("_", " ")
            : snapshot.total_count === 0
              ? "no tasks"
              : snapshot.total_count === snapshot.tasks.length
                ? `${completed} of ${snapshot.total_count} done`
                : plural(snapshot.total_count, "task"),
        openByDefault: true,
        detail:
          snapshot !== null
            ? {
                kind: "tasks",
                tasks: snapshot.tasks,
                total: snapshot.total_count,
                changedTaskId: action.task_id,
                change: taskChange(action.operation, action.status),
              }
            : action.description === null
              ? NO_DETAIL
              : { kind: "text", text: canonicalText(action.description) },
      };
    }

    case "plan":
      return {
        ...base,
        verb: "Plan",
        object: [
          part(`${action.operation} `, "dir"),
          part(action.title ?? action.plan_id ?? "", action.title === null ? "aside" : "plain"),
        ],
        result: action.step_count === null ? null : plural(action.step_count, "step"),
        detail:
          action.summary === null
            ? NO_DETAIL
            : { kind: "text", text: canonicalText(action.summary) },
      };

    case "finding": {
      const worst = action.findings.reduce(
        (highest, item) =>
          SEVERITY_ORDER.indexOf(item.severity) > SEVERITY_ORDER.indexOf(highest)
            ? item.severity
            : highest,
        "info" as FindingSeverity,
      );
      return {
        ...base,
        verb: "Reported",
        object: [part(plural(action.total_count, "finding"))],
        result: action.findings.length === 0 ? null : worst,
        openByDefault: true,
        detail: { kind: "findings", findings: action.findings, total: action.total_count },
      };
    }

    case "structured_output": {
      const document = action.document;
      const title =
        document.title ??
        (document.type === "member"
          ? document.preview
          : document.summary === null
            ? null
            : firstLine(canonicalText(document.summary)));
      return {
        ...base,
        verb: "Produced",
        object: [part(title ?? "a document", title === null ? "aside" : "plain")],
        result: null,
        openByDefault: true,
        detail: { kind: "document", document },
      };
    }

    case "external": {
      const outcome = action.fields.find((field) => field.role === "outcome");
      return {
        ...base,
        verb: action.provider ?? "External",
        object: [
          part(`${action.operation} `, "dir"),
          part(action.subject ?? "", action.subject === null ? "aside" : "plain"),
        ],
        result: outcome === undefined ? null : firstLine(displayValueText(outcome.value), 44),
        detail:
          action.fields.length === 0 && action.resources.length === 0
            ? NO_DETAIL
            : {
                kind: "fields",
                fields: action.fields,
                resources: pathFacts(action.resources, context),
              },
      };
    }

    case "unknown":
      // Honest, and the row that tells you a projector is missing. The native
      // name is the verb; `ledger.ts` appends the status word to the object and
      // sets the muted state that italicises it.
      return {
        ...base,
        verb: native.name,
        object: [part("no detail recorded")],
        result: null,
        detail: NO_DETAIL,
      };
  }
}

/* ── diffstat ────────────────────────────────────────────────────────────── */

function sumOrNull(values: readonly (number | null)[]): number | null {
  const known = values.filter((value): value is number => value !== null);
  return known.length === 0 ? null : known.reduce((total, value) => total + value, 0);
}

/**
 * `+18 −4`, and `+412` when nothing was removed — the record never writes a
 * zero it did not measure. Null when the source kept no counts at all, which
 * the caller renders as `no diff recorded`.
 */
export function diffstatText(added: number | null, removed: number | null): string | null {
  const plus = added !== null && added > 0 ? `+${added}` : null;
  const minus = removed !== null && removed > 0 ? `−${removed}` : null;
  if (plus === null && minus === null) return null;
  return [plus, minus].filter((piece) => piece !== null).join(" ");
}
