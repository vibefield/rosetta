// T-1 / §03 A, H — the prompt: the ONE filled container in the document.
//
// `--vf-fill`, radius 10, primary ink, content-hugging on the 68ch measure,
// and no eyebrow: the panel IS the label. When a page of prompts and replies
// scans in one pass it is because exactly one thing on it is filled.
//
// Two variants the record draws:
//   · attachments are tiles INSIDE the panel (16:10, radius 8, hairline), with
//     the facts in tabular figures beneath. A missing image is the dashed tile
//     with what is known — absence stated, never faked (§06).
//   · a prompt that is a local command wears a PILL instead of a panel, and
//     its `command_output` rides beside it in the caption ramp. The pair is one
//     object, which is why `turns.ts` absorbed the notice rather than leaving
//     it to draw as a second marker.
import type { AttachmentFact, Prompt as PromptModel } from "../../lib/transcript";
import { firstLine } from "../../lib/transcript";
import { fmtBytes } from "../../lib/format";
import { client } from "../../lib/rosetta";
import { Anchor } from "./Anchor";
import { Prose, SegmentActs } from "./Prose";

/** The bytes the daemon kept, as a URL this renderer may actually load. */
function assetUrl(fact: AttachmentFact): string | null {
  const source = fact.ref?.source;
  if (source === undefined || source === null || source.type !== "asset") return null;
  const url = source.asset.url;
  return /^https?:\/\//i.test(url) ? url : `${client.baseUrl}${url}`;
}

function captionOf(fact: AttachmentFact): string {
  const size = fact.byteLen === null ? null : fmtBytes(fact.byteLen);
  return [fact.caption, size].filter((piece) => piece !== null && piece !== "").join(" · ");
}

function Tile({ fact }: { readonly fact: AttachmentFact }) {
  const url = fact.kind === "image" ? assetUrl(fact) : null;
  const caption = captionOf(fact);
  return (
    <figure>
      {url === null ? (
        // §06's placeholder rule: the dashed tile with its facts, never the
        // injected `[Image: …]` sentence the pipeline let through.
        <div className="tr-att__tile tr-att__tile--missing">
          {fact.name ?? `${fact.kind} · not stored`}
        </div>
      ) : (
        <a className="tr-att__tile" href={url} target="_blank" rel="noreferrer" title="Open">
          <img src={url} alt={fact.name ?? ""} loading="lazy" decoding="async" />
        </a>
      )}
      {caption !== "" && <figcaption>{caption}</figcaption>}
    </figure>
  );
}

export function Attachments({ facts }: { readonly facts: readonly AttachmentFact[] }) {
  if (facts.length === 0) return null;
  return (
    <div className="tr-att">
      {facts.map((fact) => (
        <Tile key={fact.key} fact={fact} />
      ))}
    </div>
  );
}

export interface PromptPanelProps {
  readonly prompt: PromptModel;
  readonly domId: string;
  readonly landed: boolean;
  readonly onReceipt: (message: string) => void;
}

/** The panel alone, so a cluster can stack several inside one segment. */
export function PromptPanel({ prompt, domId, landed, onReceipt }: PromptPanelProps) {
  return (
    <div
      className={landed ? "tr-prompt is-landed" : "tr-prompt"}
      id={domId}
      data-timeline-id={prompt.row.timeline_id}
    >
      {prompt.text === null ? (
        prompt.attachments.length === 0 && (
          <p className="tr-quiet">no text recorded</p>
        )
      ) : (
        <Prose text={prompt.text} onReceipt={onReceipt} />
      )}
      <Attachments facts={prompt.attachments} />
    </div>
  );
}

export interface PromptProps {
  /** One prompt, or a cluster of consecutive prompts inside five minutes. */
  readonly prompts: readonly PromptModel[];
  readonly sessionId: string;
  readonly domIdOf: (timelineId: string) => string;
  readonly landedTimelineId: string | null;
  readonly onReceipt: (message: string) => void;
}

export function Prompt({
  prompts,
  sessionId,
  domIdOf,
  landedTimelineId,
  onReceipt,
}: PromptProps) {
  const first = prompts[0];
  if (first === undefined) return null;

  const clustered = prompts.length > 1;
  // The anchor sits against the first thing in the segment, and a pill is
  // half a panel's height — so the offset follows what actually opens it.
  const allCommands = prompts.every((prompt) => prompt.isCommand && prompt.command !== null);
  const text = prompts
    .map((prompt) => prompt.text)
    .filter((value): value is string => value !== null)
    .join("\n\n");
  return (
    <div
      className={clustered ? "tr-seg tr-cluster" : "tr-seg"}
      data-prompt={allCommands ? undefined : ""}
      data-command={allCommands ? "" : undefined}
      data-timeline-id={first.row.timeline_id}
    >
      <Anchor
        at={first.at}
        sessionId={sessionId}
        timelineId={first.row.timeline_id}
        onReceipt={onReceipt}
      />
      {prompts.map((prompt) =>
        // §03 H — a prompt that is a local command wears a pill instead of a
        // panel, and its `command_output` rides beside it. One object.
        prompt.isCommand && prompt.command !== null ? (
          <div className="tr-cmd" key={prompt.key} id={domIdOf(prompt.row.timeline_id)}>
            <code>{prompt.command}</code>
            {prompt.commandOutput !== null && <span>{firstLine(prompt.commandOutput, 160)}</span>}
          </div>
        ) : (
          <PromptPanel
            key={prompt.key}
            prompt={prompt}
            domId={domIdOf(prompt.row.timeline_id)}
            landed={landedTimelineId === prompt.row.timeline_id}
            onReceipt={onReceipt}
          />
        ),
      )}
      {!allCommands && <SegmentActs text={text} onReceipt={onReceipt} />}
    </div>
  );
}
