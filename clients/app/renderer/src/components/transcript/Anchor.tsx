// T-4 — every segment stays pointable-at.
//
// Decided Q3 A: the time lives in the MARGIN when the column has one, and is
// revealed on hover when it does not. That is a container query, not a media
// query, because the margin appears and disappears with the rail, not with the
// window — `.tr-col` is the container and `transcript.css` holds the rule.
//
// The anchor keeps its precision: a pill says the day and the minute of
// resumption, the anchor keeps HH:MM:SS. Time decomposes — day, sitting,
// second — and no level repeats another's fact.
import { clock } from "../../lib/format";

export interface AnchorProps {
  readonly at: number | null;
  /** `<session>#<timeline_id>` — what the clipboard gets. */
  readonly sessionId: string;
  readonly timelineId: string;
  readonly onReceipt: (text: string) => void;
}

export function Anchor({ at, sessionId, timelineId, onReceipt }: AnchorProps) {
  if (at === null) return null;
  const stamp = clock(at);
  return (
    <button
      type="button"
      className="tr-anchor"
      title="Copy an anchor to this message"
      onClick={() => {
        navigator.clipboard
          .writeText(`${sessionId}#${timelineId}`)
          .then(() => onReceipt(`Anchor copied · ${stamp}`))
          .catch(() => onReceipt("The clipboard refused the anchor"));
      }}
    >
      {stamp}
    </button>
  );
}
