// §03 G — generated continuation context belongs to the system marker.
import { useEffect, useState } from "react";
import type { CanonicalText } from "@rosetta/protocol";
import { useReadingSession, useRoom } from "../../room-context";
import { MessagePreview } from "./MessagePreview";
import { LineChevron } from "./glyphs";
import { useRowDisclosure } from "./disclosure-state";

/** Loaded only on disclosure. Oversized summaries are content-addressed;
 * their preview must never be presented or copied as the complete recap. */
function SummaryBody({
  canonical,
  messageKey,
  matched,
  onReceipt,
}: {
  readonly canonical: CanonicalText;
  readonly messageKey: string;
  readonly matched: boolean;
  readonly onReceipt: (message: string) => void;
}) {
  const [loaded, setLoaded] = useState<{ text: string | null; error: string | null } | null>(null);
  const [attempt, setAttempt] = useState(0);
  const { sessions } = useRoom();
  const readingSession = useReadingSession();
  useEffect(() => {
    if (canonical.type === "inline") return;
    const controller = new AbortController();
    setLoaded(null);
    const read = readingSession === null
      ? Promise.reject(new Error("No session is open to read the stored summary"))
      : sessions.run(readingSession, (client) => client.readTextAsset(canonical.asset, controller.signal));
    void read.then(
      (text) => {
        if (!controller.signal.aborted) setLoaded({ text, error: null });
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        setLoaded({
          text: null,
          error: error instanceof Error ? error.message : "The stored summary is unavailable",
        });
      },
    );
    return () => controller.abort();
  }, [canonical, attempt, sessions, readingSession]);

  const text = canonical.type === "inline" ? canonical.text : loaded?.text ?? null;
  return (
    <div className="tr-compaction__body">
      <div className="tr-compaction__byline">
        <span>Agent-generated context for continuing this session.</span>
        {text !== null && (
          <button type="button" onClick={() => {
            navigator.clipboard.writeText(text)
              .then(() => onReceipt("Copied summary"))
              .catch(() => onReceipt("The clipboard refused it"));
          }}>
            copy summary
          </button>
        )}
      </div>
      {text !== null ? (
        <div className="tr-prose ah-syntax">
          <MessagePreview messageKey={messageKey} role="agent" title="Compaction summary"
            matched={matched}
            text={text} onReceipt={onReceipt} />
        </div>
      ) : loaded?.error ? (
        <p className="tr-compaction__status" role="status">
          Could not load the full summary. {loaded.error}{" "}
          <button type="button" onClick={() => setAttempt(attempt + 1)}>Retry</button>
        </p>
      ) : (
        <p className="tr-compaction__status" role="status">Loading full summary…</p>
      )}
    </div>
  );
}

export function Compaction({
  text,
  domId,
  timelineId,
  hit,
  forceOpen,
  onReceipt,
}: {
  readonly text: CanonicalText;
  readonly domId: string;
  readonly timelineId: string;
  readonly hit: boolean;
  readonly forceOpen: boolean;
  readonly onReceipt: (message: string) => void;
}) {
  const [open, setOpen] = useRowDisclosure(domId, forceOpen || hit);
  useEffect(() => {
    if (forceOpen || hit) setOpen(true);
  }, [forceOpen, hit]);
  return (
    <details
      className="tr-compaction"
      id={domId}
      data-timeline-id={timelineId}
      data-hit={hit || undefined}
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="tr-mark">
        <span className="tr-compaction__label">
          <span className="chev"><LineChevron /></span>
          Context compacted
          <span className="tr-compaction__hint">· summary</span>
        </span>
      </summary>
      {open && (
        <SummaryBody
          key={text.type === "member" ? text.asset.digest : "inline"}
          canonical={text}
          messageKey={domId}
          matched={hit}
          onReceipt={onReceipt}
        />
      )}
    </details>
  );
}
