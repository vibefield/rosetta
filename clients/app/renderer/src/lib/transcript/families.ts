// §04 — the vocabulary, family by family. Twenty-eight closed families, one
// sentence each: `verb · object · result`, plus the glyph and what the line
// opens into.
//
// This module knows nothing about status, folding or time. It projects the
// ACTION and only the action; `ledger.ts` wraps it in the evidence (T-5's hue,
// the stamps, the fold). Splitting them is what keeps the failure rules in one
// place instead of twenty-eight.
import type { ResourceRef, ToolAction } from "@rosetta/protocol";
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
  urlHost,
} from "./vocabulary.ts";

/* ── detail payloads ─────────────────────────────────────────────────────── */

/** One pressable path in a detail — pre-split so the list draws like a line. */
export interface PathFact {
  readonly label: string;
  readonly dir: string;
  readonly name: string;
  readonly resource: ResourceRef;
}

/** A web result. The protocol keeps url and title only; the HOST is derived,
 * and it is the key the renderer caches `https://<host>/favicon.ico` under. */
export interface SourceFact {
  readonly url: string;
  readonly title: string | null;
  readonly host: string;
}

/** A fetch's honest triple: final url, title, status — never the body. */
export interface FetchFact {
  readonly url: string | null;
  readonly title: string | null;
  readonly status: number | null;
  readonly host: string;
}

/** One command inside a `command` or `code` detail. A fold carries several. */
export interface CommandFact {
  readonly command: string;
  readonly cwd: string | null;
  readonly language: string | null;
  readonly background: boolean;
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
  readonly objective: string;
  readonly background: boolean;
  /** The child was never indexed; the record says draw its summary as prose. */
  readonly orphanSummary: string | null;
}

/**
 * What a ledger line opens into (§04's last column). `none` means the line has
 * nothing honest to show — a read keeps no content by rule, and an `unknown`
 * row keeps nothing at all.
 */
export type LedgerDetail =
  | { readonly kind: "none" }
  | { readonly kind: "paths"; readonly paths: readonly PathFact[]; readonly total: number }
  | {
      readonly kind: "locations";
      readonly locations: readonly SourceLocation[];
      readonly total: number;
    }
  | { readonly kind: "sources"; readonly sources: readonly SourceFact[]; readonly total: number }
  | { readonly kind: "fetch"; readonly fetch: FetchFact }
  | { readonly kind: "diff"; readonly deltas: readonly FileDelta[]; readonly diffstat: Diffstat }
  | { readonly kind: "command"; readonly commands: readonly CommandFact[] }
  | { readonly kind: "code"; readonly commands: readonly CommandFact[] }
  | { readonly kind: "names"; readonly names: readonly string[]; readonly total: number }
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

function pathFact(resource: ResourceRef, context: LedgerContext): PathFact {
  const label = resourceDisplay(resource, context);
  const { dir, name } = splitPath(label);
  return { label, dir, name, resource };
}

function pathFacts(resources: readonly ResourceRef[], context: LedgerContext): PathFact[] {
  return resources.map((resource) => pathFact(resource, context));
}

/** `L1–180` · `p3–4`. The record puts a read's range in the RESULT slot. */
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

/** Object parts for `N things · a, b, c` — a count, then the names as an aside. */
function countAndNames(count: number, noun: string, names: readonly string[]): ObjectPart[] {
  const parts: ObjectPart[] = [part(plural(count, noun))];
  if (names.length > 0) parts.push(part(` · ${names.join(", ")}`, "aside"));
  return parts;
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
  create: "Created",
  delete: "Deleted",
  list: "Listed",
};

const SEVERITY_ORDER = ["info", "low", "medium", "high", "critical"] as const;

/**
 * The §04 table as one function. `native` is only read by the `unknown` family,
 * whose whole point is to name the tool the projector could not read.
 */
export function projectAction(
  action: ToolAction,
  native: NativeToolRef,
  context: LedgerContext = {},
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
      const facts = pathFacts(
        action.targets.map((target) => target.resource),
        context,
      );
      const range = action.targets.find((target) => target.range !== null)?.range ?? null;
      const media = action.targets.find((target) => target.media_kind !== null)?.media_kind ?? null;
      const object =
        facts.length > 1
          ? countAndNames(facts.length, "file", facts.map((fact) => fact.name))
          : facts.length === 1
            ? pathParts(facts[0].label)
            : [part("no target recorded", "aside")];
      return {
        ...base,
        verb: "Read",
        object,
        result: joined([
          range === null ? null : rangeText(range),
          action.outcome?.truncated ? "truncated" : null,
          media,
        ]),
        // Reads keep no content by rule; only the paths are pressable.
        detail:
          facts.length > 1 ? { kind: "paths", paths: facts, total: facts.length } : NO_DETAIL,
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
        title: source.title,
        host: urlHost(source.url),
      }));
      const query = action.query ?? action.allowed_domains.join(", ");
      return {
        ...base,
        verb: "Searched the web",
        object: query ? [part(query, "query")] : [part("no query recorded", "aside")],
        result: outcome === null ? null : plural(outcome.total_count, "source"),
        detail:
          sources.length === 0
            ? NO_DETAIL
            : { kind: "sources", sources, total: outcome?.total_count ?? sources.length },
      };
    }

    case "web_fetch": {
      const label = resourceLabel(action.url);
      const outcome = action.outcome;
      const status = outcome?.http_status ?? null;
      return {
        ...base,
        verb: "Fetched",
        object: pathParts(label),
        result: status === null ? null : `HTTP ${status}`,
        detail: {
          kind: "fetch",
          fetch: {
            url: outcome?.final_url ?? label,
            title: outcome?.title ?? null,
            status: outcome?.http_status ?? null,
            host: urlHost(outcome?.final_url ?? label),
          },
        },
      };
    }

    case "web_navigate": {
      const label = action.url ? resourceLabel(action.url) : (action.pattern ?? "");
      const outcome = action.outcome;
      const status = outcome?.http_status ?? null;
      return {
        ...base,
        verb: action.operation === "find_in_page" ? "Found in page" : "Opened",
        object: label ? pathParts(label) : [part("no target recorded", "aside")],
        result: status === null ? null : `HTTP ${status}`,
        detail: {
          kind: "fetch",
          fetch: {
            url: outcome?.final_url ?? (label || null),
            title: outcome?.title ?? null,
            status: outcome?.http_status ?? null,
            host: urlHost(outcome?.final_url ?? label),
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
        result: stat ?? "no diff recorded",
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
              language: null,
              background: action.background,
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
              language: action.language,
              background: false,
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
        verb: "Delegated",
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
      const text =
        action.objective ?? action.arguments ?? null;
      return {
        ...base,
        verb: "Ran workflow",
        object: label === null ? [part("unnamed", "aside")] : pathParts(label),
        result: action.background ? "in background" : null,
        detail: text === null ? NO_DETAIL : { kind: "text", text: canonicalText(text) },
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
          action.description === null
            ? action.resources.length === 0
              ? NO_DETAIL
              : {
                  kind: "paths",
                  paths: pathFacts(action.resources, context),
                  total: action.resources.length,
                }
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
      const subject =
        action.subject ??
        (action.description === null ? null : firstLine(canonicalText(action.description)));
      return {
        ...base,
        verb: "Task",
        object: [
          part(`${action.operation} `, "dir"),
          part(subject ?? action.task_id ?? "", subject === null ? "aside" : "plain"),
        ],
        result: action.status === null ? null : action.status.replaceAll("_", " "),
        detail:
          action.description === null
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
