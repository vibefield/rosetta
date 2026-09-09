import type { ReactNode } from "react";
import { useState } from "react";
import type { CanonicalText } from "@rosetta/protocol";
import type { AttachmentFact } from "../../lib/transcript";
import { Attachments, attachmentStatus } from "./Attachments";
import { canOpenAttachment } from "./attachment-file-source";
import { MessagePreview } from "./MessagePreview";
import { Prose } from "./Prose";

/** One reading of text and images, shared by prompts, replies and full messages. */
export function AuthoredMessage({ text, recordedText, attachments, messageKey, role, onReceipt, onOpenAttachment, preview = true, renderText }: {
  readonly text: string | null;
  readonly recordedText?: CanonicalText | null;
  readonly attachments: readonly AttachmentFact[];
  readonly messageKey: string;
  readonly role: "user" | "agent";
  readonly onReceipt: (text: string) => void;
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
  readonly preview?: boolean;
  readonly renderText?: (content: ReactNode) => ReactNode;
}) {
  const [reveal, setReveal] = useState<{ key: string; request: number } | null>(null);
  const open = (fact: AttachmentFact) => {
    if (canOpenAttachment(fact)) onOpenAttachment(fact);
    else {
      setReveal(old => ({ key: fact.key, request: (old?.request ?? 0) + 1 }));
      onReceipt(attachmentStatus(fact));
    }
  };
  const prose = { text: text ?? "", imageAttachments: attachments, onOpenAttachment: open, onReceipt };
  const content = (text !== null || recordedText?.type === "member") ? (preview
      ? <MessagePreview {...prose} messageKey={messageKey} role={role} recordedText={recordedText} />
      : <Prose {...prose} />) : null;
  return <>
    {renderText ? renderText(content) : content}
    <Attachments facts={attachments} onOpenAttachment={open} reveal={reveal} />
  </>;
}
