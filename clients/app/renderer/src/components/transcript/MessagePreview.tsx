import { createContext, useContext, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { FullMessage } from "./message-file-source";
import { MESSAGE_PREVIEW_CHARACTERS, messageExcerpt } from "./message-preview";
import { Prose } from "./Prose";
import type { ProseProps } from "./Prose";
import "../../styles/message-preview.css";

/** The column owns one viewer, including messages inside nested worker runs. */
export const MessageViewerContext = createContext<((message: FullMessage) => void) | null>(null);

export function MessagePreview({
  messageKey,
  role,
  title,
  recordedText,
  ...prose
}: ProseProps & { readonly messageKey: string; readonly role: FullMessage["role"]; readonly title?: string; readonly recordedText?: FullMessage["recordedText"] }) {
  const openMessage = useContext(MessageViewerContext);
  const { text } = prose;
  const member = recordedText?.type === "member" ? recordedText : null;
  const viewportRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const widthRef = useRef(0);
  const [fit, setFit] = useState({ text, budget: MESSAGE_PREVIEW_CHARACTERS });
  const budget = fit.text === text ? fit.budget : MESSAGE_PREVIEW_CHARACTERS;
  const excerpt = useMemo(() => messageExcerpt(text, budget), [text, budget]);

  useLayoutEffect(() => {
    const viewport = viewportRef.current;
    const content = contentRef.current;
    if (viewport === null || content === null || openMessage === null) return;
    const measureWidth = viewport.closest(".tr-seg") ?? viewport;
    const measure = () => {
      const width = measureWidth.clientWidth;
      if (width === 0) return; // An unopened worker ladder has no measure yet.
      if (widthRef.current > 0 && Math.abs(width - widthRef.current) > 1) {
        widthRef.current = width;
        setFit({ text, budget: MESSAGE_PREVIEW_CHARACTERS });
        return;
      }
      widthRef.current = width;
      const limit = Number.parseFloat(getComputedStyle(viewport).maxHeight);
      const height = content.offsetHeight;
      if (!Number.isFinite(limit) || height <= limit + 1 || excerpt.text.length === 0) return;
      // Remove text from the rendered tree, rather than leave invisible links
      // and copy buttons behind a CSS clip. Shortening always makes progress;
      // headings, lists, tables and code all count toward the same height.
      const next = Math.max(0, Math.floor(excerpt.text.length * limit / height) - 48);
      setFit({ text, budget: next });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(measureWidth);
    observer.observe(content);
    return () => observer.disconnect();
  }, [text, excerpt.text, openMessage]);

  // A document rendered without its column still has readable, complete text.
  // Production documents always inherit the column's viewer capability.
  if (openMessage === null) return <Prose {...prose} />;

  return (
    <div className="tr-message" data-omitted={excerpt.omitted || member !== null || undefined}>
      <div className="tr-message__viewport" ref={viewportRef}>
        <div className="tr-message__content" ref={contentRef}>
          <Prose {...prose} text={excerpt.text} imageMarkerSource={text} excerpt={excerpt.omitted || member !== null} />
        </div>
      </div>
      {(excerpt.omitted || member !== null) && (
        <div className="tr-message__footer">
          <button
            type="button"
            className="tr-message__open"
            aria-haspopup="dialog"
            onClick={() => openMessage({ key: messageKey, role, title, text, recordedText, attachments: prose.imageAttachments })}
          >
            View full message
          </button>
        </div>
      )}
    </div>
  );
}
