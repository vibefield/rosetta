// §03 C/E — the reply's own words.
//
// T-1 says the reply is BARE: no fill, no border, primary ink at 14/1.55 on
// the 68ch measure. The markdown renderer is the one the file viewer already
// uses (a tree of React elements, never `dangerouslySetInnerHTML`); the only
// addition the record asks for is the code bar — the fence's language on the
// left, a copy act on the right.
//
// The fences are split out here rather than inside `MarkdownBlocks` because
// that renderer is shared with the file viewer, where a bar over every fence
// in a README would be chrome the reader did not ask for. A fence nested in a
// list or a quote keeps the plain rendering: the bar belongs to a block that
// owns the measure, and one that does not is not that.
import { useMemo, useState } from "react";
import type { ReactNode } from "react";
import type { AttachmentFact } from "../../lib/transcript";
import { attachmentMentions } from "../../lib/transcript";
import { parseMarkdown } from "../file-viewer/markdown";
import type { Block } from "../file-viewer/markdown";
import { MarkdownBlocks } from "../file-viewer/markdown/render";
import { highlight, languageFor, PLAIN_TEXT } from "../file-viewer/syntax";

/** The hover acts. Today the room has exactly one honest act on a segment —
 * copy — so that is what it draws; the record's chip and canvas belong to
 * vibe-field, which this room is not. */
export function SegmentActs({
  text,
  onReceipt,
}: {
  readonly text: string;
  readonly onReceipt: (message: string) => void;
}) {
  if (text.trim() === "") return null;
  return (
    <div className="tr-acts">
      <button
        type="button"
        onClick={() => {
          navigator.clipboard
            .writeText(text)
            .then(() => onReceipt("Copied"))
            .catch(() => onReceipt("The clipboard refused it"));
        }}
      >
        copy
      </button>
    </div>
  );
}

function CodeBar({
  language,
  text,
  onReceipt,
  excerpt = false,
}: {
  readonly language: string | null;
  readonly text: string;
  readonly onReceipt: (message: string) => void;
  readonly excerpt?: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const lines = useMemo(() => {
    const resolved = language === null ? PLAIN_TEXT : (languageFor(`x.${language}`) ?? PLAIN_TEXT);
    return highlight(text, resolved);
  }, [language, text]);
  return (
    <div className="tr-code">
      <div className="tr-code__bar">
        <span>{language ?? "text"}</span>
        {!excerpt && <button
          type="button"
          onClick={() => {
            navigator.clipboard
              .writeText(text)
              .then(() => {
                setCopied(true);
                onReceipt("Copied");
                window.setTimeout(() => setCopied(false), 1200);
              })
              .catch(() => onReceipt("The clipboard refused it"));
          }}
        >
          {copied ? "copied" : "copy"}
        </button>}
      </div>
      <pre>
        <code>
          {lines.map((line, row) => (
            <span key={row}>
              {row > 0 && "\n"}
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
    </div>
  );
}

/** Runs of ordinary blocks go to the shared renderer; a top-level fence gets
 * the bar. Splitting rather than mapping keeps list spacing intact. */
function partition(blocks: readonly Block[]): (readonly Block[] | Extract<Block, { type: "code" }>)[] {
  const out: (readonly Block[] | Extract<Block, { type: "code" }>)[] = [];
  let run: Block[] = [];
  for (const block of blocks) {
    if (block.type === "code") {
      if (run.length > 0) {
        out.push(run);
        run = [];
      }
      out.push(block);
      continue;
    }
    run.push(block);
  }
  if (run.length > 0) out.push(run);
  return out;
}

export interface ProseProps {
  readonly text: string;
  readonly onReceipt: (message: string) => void;
  readonly imageAttachments?: readonly AttachmentFact[];
  /** Ranges are relative to the complete message, including in a preview. */
  readonly imageMarkerSource?: string;
  readonly onOpenAttachment?: (fact: AttachmentFact) => void;
  /** An excerpt may end inside a fence; do not offer to copy partial code. */
  readonly excerpt?: boolean;
}

function linkedAttachments(
  value: string,
  source: readonly number[] | undefined,
  targets: ReturnType<typeof attachmentMentions>,
  onOpen: (fact: AttachmentFact) => void,
): ReactNode {
  if (source === undefined) return value;
  const nodes: ReactNode[] = [];
  let cursor = 0;
  for (const target of targets) {
    const start = source.indexOf(target.start);
    const length = target.end - target.start;
    // Only a complete, contiguous plain-text span can become a button. The
    // parser never calls this for code or link labels; trims and escapes keep
    // source positions instead of guessing from repeated rendered wording.
    if (start < cursor || start + length > value.length || !source.slice(start, start + length).every((at, i) => at === target.start + i)) continue;
    if (start > cursor) nodes.push(value.slice(cursor, start));
    const label = value.slice(start, start + length);
    nodes.push(<button className="tr-image-link" type="button" key={target.start}
      aria-label={`${label} — ${target.target.missing ? "image unavailable" : "open image"}`}
      aria-haspopup={target.target.missing ? undefined : "dialog"}
      title={`${target.target.missing ? "Image unavailable" : "Open image"}${target.target.name ? ` · ${target.target.name}` : ""}`}
      onClick={() => onOpen(target.target)}>{label}</button>);
    cursor = start + length;
  }
  if (cursor === 0) return value;
  if (cursor < value.length) nodes.push(value.slice(cursor));
  return nodes;
}

export function Prose({
  text,
  onReceipt,
  imageAttachments,
  imageMarkerSource = text,
  onOpenAttachment,
  excerpt = false,
}: ProseProps) {
  const trackSource = imageAttachments?.some(fact => fact.ref?.message_ranges?.length) ?? false;
  const parts = useMemo(() => partition(parseMarkdown(text, trackSource)), [text, trackSource]);
  const imageTargets = useMemo(
    () => attachmentMentions(imageMarkerSource, imageAttachments ?? []),
    [imageMarkerSource, imageAttachments],
  );
  const renderText =
    imageTargets.length > 0 && onOpenAttachment !== undefined
      ? (value: string, source?: readonly number[]) => linkedAttachments(value, source, imageTargets, onOpenAttachment)
      : undefined;
  return (
    <>
      {parts.map((part, at) =>
        Array.isArray(part) ? (
          <MarkdownBlocks key={at} blocks={part} renderText={renderText} />
        ) : (
          <CodeBar
            key={at}
            language={(part as Extract<Block, { type: "code" }>).language}
            text={(part as Extract<Block, { type: "code" }>).text}
            onReceipt={onReceipt}
            excerpt={excerpt}
          />
        ),
      )}
    </>
  );
}
