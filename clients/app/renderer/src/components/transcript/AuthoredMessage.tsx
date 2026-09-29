import type { ReactNode } from "react";
import { useContext } from "react";
import type { CanonicalText } from "@rosetta/protocol";
import type { AttachmentFact } from "../../lib/transcript";
import { ActivityImagesContext, Attachments, useAttachmentActions } from "./Attachments";
import { MessagePreview } from "./MessagePreview";
import { Prose } from "./Prose";

/** One reading of text and images, shared by prompts, replies and full messages. */
export function AuthoredMessage({ text, recordedText, attachments, messageKey, role, onReceipt, onOpenAttachment, preview = true, renderText, imagesOutside = false, title, matched }: {
  readonly text: string | null;
  readonly recordedText?: CanonicalText | null;
  readonly attachments: readonly AttachmentFact[];
  readonly messageKey: string;
  readonly role: "user" | "agent";
  readonly onReceipt: (text: string) => void;
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
  readonly preview?: boolean;
  readonly imagesOutside?: boolean;
  readonly title?: string;
  readonly matched?: boolean;
  readonly renderText?: (content: ReactNode) => ReactNode;
}) {
  const actions = useAttachmentActions(onOpenAttachment, onReceipt);
  const gathered = useContext(ActivityImagesContext);
  const shared = imagesOutside ? gathered : null;
  const open = (fact: AttachmentFact) => (shared && fact.kind === "image" ? shared : actions.open)(fact);
  const prose = { text: text ?? "", imageAttachments: attachments, onOpenAttachment: open, onReceipt };
  const content = (text !== null || recordedText?.type === "member") ? (preview
      ? <MessagePreview {...prose} messageKey={messageKey} role={role} recordedText={recordedText} title={title} matched={matched} />
      : <Prose {...prose} />) : null;
  return <>
    {renderText ? renderText(content) : content}
    <Attachments facts={shared ? attachments.filter(fact => fact.kind !== "image") : attachments} onOpenAttachment={open} reveal={actions.reveal} />
  </>;
}
