// T-1 / §03 A, H — the prompt: the ONE filled container in the document.
//
// `--vf-fill`, radius 10, primary ink, content-hugging on the 68ch measure,
// and no eyebrow: the panel IS the label. When a page of prompts and replies
// scans in one pass it is because exactly one thing on it is filled.
//
// Two variants the record draws:
//   · attachments sit in one scrolling strip BELOW the text panel. Missing
//     images keep their place; image-only prompts need no empty bubble (§06A).
//   · a prompt that is a local command wears a PILL instead of a panel, and
//     its `command_output` rides beside it in the caption ramp. The pair is one
//     object, which is why `turns.ts` absorbed the notice rather than leaving
//     it to draw as a second marker. A setting command is the tighter form of
//     that object: label + normalized value, with the CLI's prose discarded.
import type { AttachmentFact, Prompt as PromptModel } from "../../lib/transcript";
import { firstLine } from "../../lib/transcript";
import { clock } from "../../lib/format";
import { Anchor } from "./Anchor";
import { AuthoredMessage } from "./AuthoredMessage";
import { MessagePreview } from "./MessagePreview";
import { SegmentActs } from "./Prose";

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
  return (
    <div
      className={landed ? "tr-prompt-message is-landed" : "tr-prompt-message"}
      id={domId}
      data-timeline-id={prompt.row.timeline_id}
    >
      <AuthoredMessage text={prompt.text} attachments={prompt.attachments}
        recordedText={prompt.row.body.type === "user" ? prompt.row.body.text : null}
        messageKey={`${prompt.row.session_id}:${prompt.row.timeline_id}`} role="user"
        onReceipt={onReceipt} onOpenAttachment={onOpenAttachment}
        renderText={content => content !== null || prompt.attachments.length === 0
          ? <div className="tr-prompt">{content ?? <p className="tr-quiet">no text recorded</p>}</div>
          : null} />
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
  const recordedText = prompt.row.body.type === "collaboration" ? prompt.row.body.event.body : null;
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
      {prompt.text === null && recordedText?.type !== "member" ? (
        <span className="tr-quiet">no message body recorded</span>
      ) : (
        <MessagePreview messageKey={`${prompt.row.session_id}:${prompt.row.timeline_id}`} role="agent"
          title={`${machine.label}${machine.sender === null ? "" : ` from ${senderLabel(machine.sender)}`}`}
          text={prompt.text ?? ""} recordedText={recordedText} onReceipt={onReceipt} />
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
