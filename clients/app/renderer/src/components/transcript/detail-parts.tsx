// The heavier detail bodies — a change's diff, a question's options, a
// report's findings, a produced document, an external call's fields, a
// collaboration's participants.
//
// Split out of `LineDetail.tsx` only for size: the dispatcher there names one
// renderer per `LedgerDetail` kind, and these are the ones long enough to
// bury it. Every rule they obey is the same one — show what the ROW proved,
// state absence rather than faking it, and cap what would otherwise become a
// scroll trap inside the measure.
import { useMemo, useState } from "react";
import type { ReactNode } from "react";
import { FileIcon } from "@vibecook/mille-ui/icons";
import { vibefieldIconTheme } from "@vibecook/mille-ui/icons/vibefield";
import type { TaskSnapshotItem } from "@rosetta/protocol";
import type { CommandFact, Diffstat, PathFact, TaskChange } from "../../lib/transcript";
import {
  canonicalText,
  commandDirectoryContext,
  commandPreviewText,
  diffstatText,
  resourceDisplay,
  splitPath,
  TASK_PREVIEW_LIMIT,
  taskDisplayRows,
} from "../../lib/transcript";
import type {
  CanonicalDocument,
  CollaboratorRef,
  DisplayField,
  DisplayValue,
  FileDelta,
  FindingItem,
  UserAnswer,
  UserQuestion,
} from "../../lib/transcript/protocol";
import { parseDiff } from "../file-viewer/diff";
import { highlight, languageFor, PLAIN_TEXT } from "../file-viewer/syntax";
import type { Language, TokenLine } from "../file-viewer/syntax";
import { client } from "../../lib/rosetta";
import { DirectoryGlyph } from "./glyphs";

/** Derived, never re-declared: a document's blocks are whatever the generated
 * `CanonicalDocument` says they are (iron rule 6). */
type DocumentBlock = Extract<CanonicalDocument, { type: "inline" }>["blocks"][number];
type DocumentScalar = Extract<DocumentBlock, { type: "list" }>["items"][number];

/** Q5 A's cap: the hunk that matters, where you are reading. */
const DIFF_LINE_CAP = 12;

/** A bounded list is a list; an unbounded one is a scroll trap in the measure. */
export const LIST_CAP = 24;

const SHELL = languageFor("command.sh") ?? PLAIN_TEXT;
const LANGUAGE_FILE: Readonly<Record<string, string>> = {
  bash: "command.sh",
  bun: "command.js",
  deno: "command.js",
  fish: "command.sh",
  javascript: "command.js",
  js: "command.js",
  lua: "command.lua",
  node: "command.js",
  nodejs: "command.js",
  php: "command.php",
  py: "command.py",
  python: "command.py",
  ruby: "command.rb",
  sh: "command.sh",
  shell: "command.sh",
  sql: "command.sql",
  sqlite3: "command.sql",
  typescript: "command.ts",
  ts: "command.ts",
  zsh: "command.sh",
};

function namedLanguage(name: string | null): Language | null {
  if (name === null) return null;
  const normalized = name.trim().toLowerCase();
  const file = LANGUAGE_FILE[normalized] ?? "command." + normalized;
  return languageFor(file);
}

function heredocLanguage(opener: string): Language | null {
  const redirected = opener.match(/>\s*["']?([^\s"';&|]+)["']?\s+<<-?/);
  if (redirected !== null) {
    const fromTarget = languageFor(redirected[1]!);
    if (fromTarget !== null) return fromTarget;
  }
  const interpreter = opener.match(
    /(?:^|[;&|]\s*)(?:env\s+)?(python(?:\d+(?:\.\d+)*)?|nodejs?|deno|bun|ruby|php|lua|bash|zsh|fish|sh|sqlite3)\b/i,
  )?.[1];
  if (interpreter === undefined) return null;
  return namedLanguage(interpreter.toLowerCase().replace(/^python.*$/, "python"));
}

/** A command is shell until a declared heredoc body begins. When its launcher
 * or redirect target names a language, use the file viewer's grammar for that
 * body and return to shell for the delimiter and anything after it. */
function commandHighlight(
  text: string,
  language: string | null,
  code: boolean,
): readonly TokenLine[] {
  if (code) return highlight(text, namedLanguage(language) ?? PLAIN_TEXT);

  const lines = text.split("\n");
  const markerMatch = lines[0]?.match(/<<-?\s*(?:'([^']+)'|"([^"]+)"|([A-Za-z_][A-Za-z0-9_]*))/);
  const embedded = markerMatch === undefined || markerMatch === null
    ? null
    : heredocLanguage(lines[0]!);
  if (markerMatch === undefined || markerMatch === null || embedded === null || lines.length < 2) {
    return highlight(text, SHELL);
  }

  const marker = markerMatch[1] ?? markerMatch[2] ?? markerMatch[3];
  const close = lines.findIndex((line, at) => at > 0 && line.trim() === marker);
  const bodyEnd = close === -1 ? lines.length : close;
  const out: TokenLine[] = [...highlight(lines[0]!, SHELL)];
  if (bodyEnd > 1) out.push(...highlight(lines.slice(1, bodyEnd).join("\n"), embedded));
  if (close !== -1) out.push(...highlight(lines.slice(close).join("\n"), SHELL));
  return out;
}

function HighlightedCommand({
  text,
  language,
  code,
}: {
  readonly text: string;
  readonly language: string | null;
  readonly code: boolean;
}) {
  const lines = useMemo(
    () => commandHighlight(text, language, code),
    [code, language, text],
  );
  return (
    <pre className={code ? "tr-diag tr-command__text" : "tr-command__text"} aria-label={text}>
      <code>
        {lines.map((line, row) => (
          <span className="tr-command__line" key={row}>
            {line.map((token, at) =>
              token.kind === "plain" ? (
                <span key={at}>{token.text}</span>
              ) : (
                <span data-t={token.kind} key={at}>
                  {token.text}
                </span>
              ),
            )}
          </span>
        ))}
      </code>
    </pre>
  );
}

export function PathList({
  paths,
  total,
  onOpenPath,
}: {
  readonly paths: readonly PathFact[];
  readonly total: number;
  readonly onOpenPath?: ((path: PathFact) => void) | undefined;
}) {
  const shown = paths.slice(0, LIST_CAP);
  const hasOpenablePath =
    onOpenPath !== undefined && shown.some((path) => path.read !== undefined);
  return (
    <ul className="tr-paths" data-openable={hasOpenablePath || undefined}>
      {shown.map((path, at) => {
        const openable = onOpenPath !== undefined && path.read !== undefined;
        const name = splitPath(path.label).name || path.name || path.label;
        const entryKind =
          path.resource.type === "directory" ? 1 : path.resource.type === "file" ? 0 : null;
        const label = (
          <>
            {entryKind !== null && (
              <FileIcon
                ariaLabel={`${name} ${entryKind === 1 ? "folder" : "file"}`}
                className="tr-paths__icon"
                entry={{ name, kind: entryKind }}
                expanded={false}
                theme={vibefieldIconTheme}
              />
            )}
            <span className="name">{name}</span>
            {path.range !== undefined && <span className="tr-paths__range">{path.range}</span>}
          </>
        );
        return (
          <li key={`${path.label}-${at}`} title={path.label}>
            {openable ? (
              <button
                type="button"
                className="tr-paths__file"
                title={path.label}
                aria-label={`Open ${path.label}${path.range === undefined ? "" : `, ${path.range}`}`}
                onClick={() => onOpenPath?.(path)}
              >
                {label}
              </button>
            ) : (
              <span className="tr-paths__entry">{label}</span>
            )}
          </li>
        );
      })}
      {total > shown.length && <li className="tr-more">… {total - shown.length} more</li>}
    </ul>
  );
}

const TASK_STATUS_LABEL: Readonly<Record<string, string>> = {
  pending: "pending",
  in_progress: "in progress",
  completed: "done",
  failed: "failed",
  cancelled: "cancelled",
  unknown: "status unknown",
};

const TASK_CHANGE_LABEL: Readonly<Record<TaskChange, string>> = {
  added: "added",
  started: "started",
  done: "done",
  reopened: "reopened",
  failed: "failed",
  cancelled: "cancelled",
  updated: "updated",
};

/** A task action opens to a three-row glimpse immediately after that action.
 * The changed row stays last; the reader can disclose the complete board. */
export function Tasks({
  tasks,
  total,
  changedTaskId,
  change,
}: {
  readonly tasks: readonly TaskSnapshotItem[];
  readonly total: number;
  readonly changedTaskId: string | null;
  readonly change: TaskChange | null;
}) {
  const [showFull, setShowFull] = useState(false);
  const all = useMemo(
    () => taskDisplayRows(tasks, changedTaskId, true),
    [tasks, changedTaskId],
  );
  const shown = useMemo(
    () => (showFull ? all : taskDisplayRows(tasks, changedTaskId, false)),
    [all, changedTaskId, showFull, tasks],
  );
  const canToggle = all.length > TASK_PREVIEW_LIMIT;
  const unrecorded = Math.max(0, total - all.length);
  return (
    <ul className="tr-tasks" aria-label="Task list">
      {shown.length === 0 && <li className="tr-tasks__empty">No tasks at this point</li>}
      {shown.map((task) => {
        const status = task.status ?? "unknown";
        const changed = task.task_id === changedTaskId ? change : null;
        const metadata = [
          `#${task.task_id}`,
          task.owner === null ? null : task.owner,
          task.blocker_ids.length === 0
            ? null
            : `blocked by ${task.blocker_ids.map((id) => `#${id}`).join(", ")}`,
        ].filter((item): item is string => item !== null);
        return (
          <li
            className="tr-task"
            data-change={changed ?? undefined}
            data-status={status}
            key={task.task_id}
          >
            <span
              aria-label={TASK_STATUS_LABEL[status]}
              className="tr-task__check"
              role="img"
              title={TASK_STATUS_LABEL[status]}
            />
            <span className="tr-task__subject" title={task.subject ?? `Task #${task.task_id}`}>
              {task.subject ?? `Task #${task.task_id}`}
            </span>
            <span className="tr-task__meta">{metadata.join(" · ")}</span>
            {changed !== null && <span className="tr-task__delta">{TASK_CHANGE_LABEL[changed]}</span>}
          </li>
        );
      })}
      {canToggle && (
        <li className="tr-tasks__toggle">
          <button
            type="button"
            aria-expanded={showFull}
            aria-label={showFull ? "Show less" : `Show all ${total} tasks`}
            onClick={() => setShowFull((value) => !value)}
          >
            {showFull ? "show less" : "show full tasks"}
          </button>
        </li>
      )}
      {showFull && unrecorded > 0 && (
        <li className="tr-more">… {unrecorded} more not recorded</li>
      )}
    </ul>
  );
}


export function DiffLines({ delta }: { readonly delta: FileDelta }) {
  const text = delta.diff === null ? "" : canonicalText(delta.diff);
  if (text.trim() === "") {
    return <p className="tr-text tr-quiet">no diff recorded</p>;
  }
  const parsed = parseDiff(text);
  const rows: ReactNode[] = [];
  let drawn = 0;
  let total = 0;
  for (const hunk of parsed.hunks) {
    for (const line of hunk.lines) {
      total += 1;
      if (drawn >= DIFF_LINE_CAP) continue;
      drawn += 1;
      const number = line.newNumber ?? line.oldNumber ?? "";
      rows.push(
        <div className={line.kind === "add" ? "add" : line.kind === "del" ? "del" : "ctx"} key={total}>
          <span>{number}</span>
          <span>{line.kind === "add" ? "+" : line.kind === "del" ? "−" : " "}</span>
          <span>{line.text}</span>
        </div>,
      );
    }
  }
  // git said something this parser could not read as hunks — the honest
  // answer is the stored text, still capped, rather than an empty frame.
  if (rows.length === 0) {
    const lines = text.split("\n");
    total = lines.length;
    lines.slice(0, DIFF_LINE_CAP).forEach((line, at) => {
      rows.push(
        <div className="ctx" key={at}>
          <span />
          <span> </span>
          <span>{line}</span>
        </div>,
      );
    });
  }
  return (
    <div className="tr-diff">
      {rows}
      {total > DIFF_LINE_CAP && <div className="more">… {total - DIFF_LINE_CAP} more lines</div>}
    </div>
  );
}

export function DiffDetail({
  deltas,
  diffstat,
}: {
  readonly deltas: readonly FileDelta[];
  readonly diffstat: Diffstat;
}) {
  const stat = diffstatText(diffstat.added, diffstat.removed);
  return (
    <>
      {deltas.slice(0, 4).map((delta, at) => (
        <div key={at}>
          <div className="tr-detail__bar">
            <span className="path">{resourceDisplay(delta.path, {})}</span>
            {at === 0 && stat !== null && <span>{stat}</span>}
          </div>
          <DiffLines delta={delta} />
        </div>
      ))}
      {deltas.length > 4 && (
        <p className="tr-text tr-quiet">… {deltas.length - 4} more files in this change</p>
      )}
    </>
  );
}

export function CommandList({
  commands,
  code,
}: {
  readonly commands: readonly CommandFact[];
  readonly code: boolean;
}) {
  const directory = commandDirectoryContext(commands);
  return (
    <>
      {directory !== null && (
        <span
          aria-label={directory.title}
          className="tr-command-cwd"
          title={directory.title}
        >
          <DirectoryGlyph />
          <bdi>{directory.label}</bdi>
        </span>
      )}
      {code ? (
        <div className="tr-command-stack">
          {commands.map((command, at) => (
            <CommandSource command={command} code key={at} />
          ))}
        </div>
      ) : (
        <ul className="tr-cmds">
          {commands.slice(0, LIST_CAP).map((command, at) => (
            <CommandSource command={command} code={false} key={at} />
          ))}
          {commands.length > LIST_CAP && (
            <li className="tr-more">… {commands.length - LIST_CAP} more</li>
          )}
        </ul>
      )}
    </>
  );
}

function CommandSource({ command, code }: { readonly command: CommandFact; readonly code: boolean }) {
  const [full, setFull] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);
  const expandable =
    command.sourceTruncated && command.sessionId !== null && command.timelineId !== null;

  const disclose = async () => {
    if (expanded) {
      setExpanded(false);
      return;
    }
    if (!expandable || loading) return;
    if (full !== null) {
      setExpanded(true);
      return;
    }
    setLoading(true);
    setFailed(false);
    try {
      const response = await client.command(command.sessionId!, command.timelineId!);
      setFull(response.command);
      setExpanded(true);
    } catch {
      setFailed(true);
    } finally {
      setLoading(false);
    }
  };

  const text =
    expanded && full !== null
      ? full
      : commandPreviewText(command.command, command.sourceTruncated);
  const body = <HighlightedCommand code={code} language={command.language} text={text} />;
  const more = expandable ? (
    <button
      aria-expanded={expanded}
      className="tr-command__more"
      type="button"
      onClick={disclose}
      disabled={loading}
    >
      {loading
        ? "loading full command…"
        : failed
          ? "couldn’t load · retry"
          : expanded
            ? "show preview"
            : "show full command"}
    </button>
  ) : null;

  return code ? (
    <div className="tr-command ah-syntax" data-full={expanded || undefined}>
      {body}
      {more}
    </div>
  ) : (
    <li className="tr-command ah-syntax" data-full={expanded || undefined}>
      <span aria-hidden="true" className="tr-command__prompt">&gt;_</span>
      {body}
      {more}
    </li>
  );
}

function answerText(question: UserQuestion, answers: readonly UserAnswer[]): string[] {
  return answers
    .filter((answer) => answer.question_id === question.id)
    .flatMap((answer) => answer.values.map(canonicalText));
}

/** Conversation, so it opens by default: the questions with their options and
 * the answer marked in primary ink. */
export function Questions({
  questions,
  answers,
}: {
  readonly questions: readonly UserQuestion[];
  readonly answers: readonly UserAnswer[];
}) {
  return (
    <dl className="tr-kv">
      {questions.map((question) => {
        const chosen = answerText(question, answers);
        const free = chosen.filter(
          (value) => !question.options.some((option) => option.id === value || option.label === value),
        );
        return (
          <div className="tr-kv__pair" key={question.id}>
            <dt>{question.header ?? canonicalText(question.prompt)}</dt>
            <dd data-prose="">
              {question.options.length === 0
                ? chosen.join(" · ") || "no answer recorded"
                : question.options.map((option, at) => {
                    const picked = chosen.includes(option.id) || chosen.includes(option.label);
                    return (
                      <span key={option.id}>
                        {at > 0 && " · "}
                        {picked ? <b>{option.label} (chosen)</b> : option.label}
                      </span>
                    );
                  })}
              {free.length > 0 && question.options.length > 0 && (
                <span>
                  {" · "}
                  <b>{free.join(", ")}</b>
                </span>
              )}
            </dd>
          </div>
        );
      })}
    </dl>
  );
}

export function Findings({ findings, total }: { readonly findings: readonly FindingItem[]; readonly total: number }) {
  return (
    <dl className="tr-kv">
      {findings.slice(0, LIST_CAP).map((finding, at) => (
        <div className="tr-kv__pair" key={at}>
          <dt>{finding.severity}</dt>
          <dd data-prose="">
            <b>{finding.title}</b>
            {finding.summary !== null && ` — ${canonicalText(finding.summary)}`}
            {finding.resources.length > 0 && (
              <span className="n">
                {finding.resources.map((resource) => resourceDisplay(resource, {})).join(", ")}
              </span>
            )}
          </dd>
        </div>
      ))}
      {total > findings.length && (
        <div className="tr-kv__pair">
          <dt />
          <dd data-prose="">… {total - findings.length} more</dd>
        </div>
      )}
    </dl>
  );
}

function displayText(value: DisplayValue): string {
  switch (value.type) {
    case "text":
    case "number":
      return value.value;
    case "boolean":
      return value.value ? "yes" : "no";
    case "resource":
      return resourceDisplay(value.value, {});
    case "list":
      return value.value
        .map((item) => (item.type === "boolean" ? (item.value ? "yes" : "no") : item.value))
        .join(", ");
  }
}

export function Fields({ fields }: { readonly fields: readonly DisplayField[] }) {
  if (fields.length === 0) return null;
  return (
    <dl className="tr-kv">
      {fields.map((field, at) => (
        <div className="tr-kv__pair" key={at}>
          <dt>{field.label}</dt>
          <dd data-prose="">{displayText(field.value)}</dd>
        </div>
      ))}
    </dl>
  );
}

function DocumentBlocks({ blocks }: { readonly blocks: readonly DocumentBlock[] }) {
  return (
    <>
      {blocks.map((block, at) => {
        switch (block.type) {
          case "field":
            return <Fields key={at} fields={[block.field]} />;
          case "list":
            return (
              <ul className="tr-paths" key={at}>
                {block.heading !== null && <li className="tr-more">{block.heading}</li>}
                {block.items.slice(0, LIST_CAP).map((item: DocumentScalar, index: number) => (
                  <li key={index}>{item.type === "boolean" ? (item.value ? "yes" : "no") : item.value}</li>
                ))}
              </ul>
            );
          case "table":
            return (
              <table className="tr-dtable" key={at}>
                {block.heading !== null && <caption>{block.heading}</caption>}
                <thead>
                  <tr>
                    {block.columns.map((column: string, index: number) => (
                      <th key={index}>{column}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {block.rows.slice(0, LIST_CAP).map((row: readonly DocumentScalar[], index: number) => (
                    <tr key={index}>
                      {row.map((cell: DocumentScalar, column: number) => (
                        <td key={column}>
                          {cell.type === "boolean" ? (cell.value ? "yes" : "no") : cell.value}
                        </td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            );
          case "section":
            return (
              <div key={at}>
                <p className="tr-text tr-quiet">{block.heading}</p>
                <DocumentBlocks blocks={block.blocks} />
              </div>
            );
        }
      })}
    </>
  );
}

export function Document({ document }: { readonly document: CanonicalDocument }) {
  if (document.type === "member") {
    return <p className="tr-text">{document.preview}</p>;
  }
  return (
    <>
      {document.summary !== null && <p className="tr-text">{canonicalText(document.summary)}</p>}
      <DocumentBlocks blocks={document.blocks} />
      {document.resources.length > 0 && (
        <PathList
          paths={document.resources.map((resource) => ({
            label: resourceDisplay(resource, {}),
            dir: "",
            name: resourceDisplay(resource, {}),
            resource,
          }))}
          total={document.resources.length}
        />
      )}
    </>
  );
}

export function Participants({
  participants,
  total,
  body,
}: {
  readonly participants: readonly CollaboratorRef[];
  readonly total: number;
  readonly body: string | null;
}) {
  return (
    <>
      {body !== null && <p className="tr-text">{body}</p>}
      <dl className="tr-kv">
        {participants.slice(0, LIST_CAP).map((participant, at) => (
          <div className="tr-kv__pair" key={at}>
            <dt>{participant.name}</dt>
            <dd data-prose="">
              {[participant.role, participant.state].filter((piece) => piece !== null).join(" · ")}
            </dd>
          </div>
        ))}
        {total > participants.length && (
          <div className="tr-kv__pair">
            <dt />
            <dd data-prose="">… {total - participants.length} more</dd>
          </div>
        )}
      </dl>
    </>
  );
}
