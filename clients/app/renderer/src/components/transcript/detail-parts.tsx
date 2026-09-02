// The heavier detail bodies — a change's diff, a question's options, a
// report's findings, a produced document, an external call's fields, a
// collaboration's participants.
//
// Split out of `LineDetail.tsx` only for size: the dispatcher there names one
// renderer per `LedgerDetail` kind, and these are the ones long enough to
// bury it. Every rule they obey is the same one — show what the ROW proved,
// state absence rather than faking it, and cap what would otherwise become a
// scroll trap inside the measure.
import type { ReactNode } from "react";
import type { CommandFact, Diffstat, PathFact } from "../../lib/transcript";
import { canonicalText, diffstatText, resourceDisplay } from "../../lib/transcript";
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

/** Derived, never re-declared: a document's blocks are whatever the generated
 * `CanonicalDocument` says they are (iron rule 6). */
type DocumentBlock = Extract<CanonicalDocument, { type: "inline" }>["blocks"][number];
type DocumentScalar = Extract<DocumentBlock, { type: "list" }>["items"][number];

/** Q5 A's cap: the hunk that matters, where you are reading. */
const DIFF_LINE_CAP = 12;

/** A bounded list is a list; an unbounded one is a scroll trap in the measure. */
export const LIST_CAP = 24;

export function PathList({ paths, total }: { readonly paths: readonly PathFact[]; readonly total: number }) {
  const shown = paths.slice(0, LIST_CAP);
  return (
    <ul className="tr-paths">
      {shown.map((path, at) => (
        <li key={`${path.label}-${at}`}>
          <span className="dir">{path.dir}</span>
          {path.name}
        </li>
      ))}
      {total > shown.length && <li className="tr-more">… {total - shown.length} more</li>}
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
  const cwd = commands.find((command) => command.cwd !== null)?.cwd ?? null;
  return (
    <>
      {code ? (
        <pre className="tr-diag">{commands.map((command) => command.command).join("\n\n")}</pre>
      ) : (
        <ul className="tr-cmds">
          {commands.slice(0, LIST_CAP).map((command, at) => (
            <li key={at}>{command.command}</li>
          ))}
          {commands.length > LIST_CAP && (
            <li className="tr-more">… {commands.length - LIST_CAP} more</li>
          )}
        </ul>
      )}
      {cwd !== null && (
        <dl className="tr-kv">
          <dt>in</dt>
          <dd>{cwd}</dd>
        </dl>
      )}
    </>
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
