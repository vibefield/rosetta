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
//     it to draw as a second marker. A setting command is the tighter form of
//     that object: label + normalized value, with the CLI's prose discarded.
import type { AttachmentFact, Prompt as PromptModel } from "../../lib/transcript";
import { firstLine } from "../../lib/transcript";
import { clock, fmtBytes } from "../../lib/format";
import { Anchor } from "./Anchor";
import { attachmentAssetUrl, canOpenAttachment } from "./attachment-file-source";
import { Prose, SegmentActs } from "./Prose";
import { MessagePreview } from "./MessagePreview";

function captionOf(fact: AttachmentFact): string {
  const size = fact.byteLen === null ? null : fmtBytes(fact.byteLen);
  return [fact.caption, size].filter((piece) => piece !== null && piece !== "").join(" · ");
}

function Tile({
  fact,
  onOpenAttachment,
}: {
  readonly fact: AttachmentFact;
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
}) {
  const url = fact.kind === "image" ? attachmentAssetUrl(fact) : null;
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
        <button
          className="tr-att__tile"
          type="button"
          title="Open image"
          aria-label={`Open image${fact.name === null ? "" : ` — ${fact.name}`}`}
          onClick={() => onOpenAttachment(fact)}
        >
          <img src={url} alt={fact.name ?? ""} loading="lazy" decoding="async" />
        </button>
      )}
      {caption !== "" && <figcaption>{caption}</figcaption>}
    </figure>
  );
}

export function Attachments({
  facts,
  onOpenAttachment,
}: {
  readonly facts: readonly AttachmentFact[];
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
}) {
  if (facts.length === 0) return null;
  return (
    <div className="tr-att">
      {facts.map((fact) => (
        <Tile key={fact.key} fact={fact} onOpenAttachment={onOpenAttachment} />
      ))}
    </div>
  );
}

export interface PromptPanelProps {
  readonly prompt: PromptModel;
  readonly domId: string;
  readonly landed: boolean;
  readonly onReceipt: (message: string) => void;
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
}

/** The panel alone, so a cluster can stack several inside one segment. */
export function PromptPanel({
  prompt,
  domId,
  landed,
  onReceipt,
  onOpenAttachment,
}: PromptPanelProps) {
  const openableImages = prompt.attachments.filter(canOpenAttachment);
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
        <MessagePreview
          messageKey={`${prompt.row.session_id}:${prompt.row.timeline_id}`}
          role="user"
          text={prompt.text}
          onReceipt={onReceipt}
          imageAttachments={openableImages}
          onOpenAttachment={onOpenAttachment}
        />
      )}
      <Attachments facts={prompt.attachments} onOpenAttachment={onOpenAttachment} />
    </div>
  );
}

export interface PromptProps {
  /** One conversational prompt, or adjacent successful setting commands. */
  readonly prompts: readonly PromptModel[];
  readonly domIdOf: (timelineId: string) => string;
  readonly landedTimelineId: string | null;
  readonly onReceipt: (message: string) => void;
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
}

function senderLabel(sender: string): string {
  if (sender === "coordinator") return "Claude Code";
  return sender.startsWith("@") ? sender : `@${sender}`;
}

function MachinePrompt({
  prompt,
  domId,
  onReceipt,
}: {
  readonly prompt: PromptModel;
  readonly domId: string;
  readonly onReceipt: (message: string) => void;
}) {
  const machine = prompt.machine;
  if (machine === null) return null;
  const facts = [
    machine.label,
    machine.sender === null ? null : `from ${senderLabel(machine.sender)}`,
    prompt.at === null ? null : clock(prompt.at),
  ].filter((fact): fact is string => fact !== null);
  return (
    <div
      className="tr-assignment"
      id={domId}
      data-timeline-id={prompt.row.timeline_id}
    >
      <span className="tr-assignment__head">{facts.join(" · ")}</span>
      {prompt.text === null ? (
        <span className="tr-quiet">no message body recorded</span>
      ) : (
        <Prose text={prompt.text} onReceipt={onReceipt} />
      )}
    </div>
  );
}

function CommandPrompt({ prompt, domId }: { readonly prompt: PromptModel; readonly domId: string }) {
  if (prompt.setting !== null) {
    return (
      <div
        className="tr-setting"
        id={domId}
        data-setting={prompt.setting.kind}
        title={prompt.command ?? undefined}
        aria-label={`${prompt.setting.label}: ${prompt.setting.value}`}
      >
        <span className="tr-setting__label">{prompt.setting.label}</span>
        <span className="tr-setting__value">{prompt.setting.value}</span>
      </div>
    );
  }
  return (
    <div className="tr-cmd" id={domId}>
      <code>{prompt.command}</code>
      {prompt.commandOutput !== null && <span>{firstLine(prompt.commandOutput, 160)}</span>}
    </div>
  );
}

export function Prompt({
  prompts,
  domIdOf,
  landedTimelineId,
  onReceipt,
  onOpenAttachment,
}: PromptProps) {
  const first = prompts[0];
  if (first === undefined) return null;

  const clustered = prompts.length > 1;
  // The anchor sits against the first thing in the segment, and a pill is
  // half a panel's height — so the offset follows what actually opens it.
  const allCommands = prompts.every((prompt) => prompt.isCommand && prompt.command !== null);
  const allMachine = prompts.every((prompt) => prompt.machine !== null);
  const text = prompts
    .map((prompt) => prompt.text)
    .filter((value): value is string => value !== null)
    .join("\n\n");
  return (
    <div
      className={clustered ? "tr-seg tr-cluster" : "tr-seg"}
      data-prompt={allCommands || allMachine ? undefined : ""}
      data-command={allCommands ? "" : undefined}
      data-machine={allMachine ? "" : undefined}
      data-timeline-id={first.row.timeline_id}
    >
      <Anchor
        at={first.at}
        sessionId={first.row.session_id}
        timelineId={first.row.timeline_id}
        onReceipt={onReceipt}
      />
      {prompts.map((prompt) =>
        // §03 H — a prompt that is a local command wears a pill instead of a
        // panel, and its `command_output` rides beside it. One object.
        prompt.machine !== null ? (
          <MachinePrompt
            key={prompt.key}
            prompt={prompt}
            domId={domIdOf(prompt.row.timeline_id)}
            onReceipt={onReceipt}
          />
        ) : prompt.isCommand && prompt.command !== null ? (
          <CommandPrompt
            key={prompt.key}
            prompt={prompt}
            domId={domIdOf(prompt.row.timeline_id)}
          />
        ) : (
          <PromptPanel
            key={prompt.key}
            prompt={prompt}
            domId={domIdOf(prompt.row.timeline_id)}
            landed={landedTimelineId === prompt.row.timeline_id}
            onReceipt={onReceipt}
            onOpenAttachment={onOpenAttachment}
          />
        ),
      )}
      {!allCommands && !allMachine && <SegmentActs text={text} onReceipt={onReceipt} />}
    </div>
  );
}
