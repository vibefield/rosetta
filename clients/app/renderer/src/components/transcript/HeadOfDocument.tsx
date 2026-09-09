// §08 — the head line, the pull bar, the failure sentences, the session start.
//
// A marker that counts, in the marker family's bare-text form, plus one
// instrument: the veil's colourless bar. The line measures the SESSION; the
// scrollbar measures the loaded document. Everything it renders comes from
// `describeHead`, so what it draws is provable without a DOM.
import { clock } from "../../lib/format";
import type { TimelineFeed } from "../../lib/hooks";
import type { HeadView } from "../../lib/transcript/head-state";
import { describeHead } from "../../lib/transcript/head-state";
import "../../styles/transcript-head.css";

/** `Mon 31 Aug · 23:18:12` — the session's first second, said as a fact.
 *  Absolute, never "Today": the start of a session is a date, not a distance. */
export function sessionStartLabel(startedAt: number | null): string | null {
  if (startedAt === null || !Number.isFinite(startedAt)) return null;
  const words = new Date(startedAt).toLocaleDateString("en-GB", {
    weekday: "short",
    day: "numeric",
    month: "short",
  });
  return `${words} · ${clock(startedAt)}`;
}

/** The top fade — one sticky, zero-height element at the head of the scroller
 *  whose pseudo-element paints the gradient. Mount it as the FIRST child of
 *  `.ah-scroll`; `useHeadOfDocument` writes `--frost` onto the scroller. */
export function TopFade() {
  return <div className="tr-frost" aria-hidden="true" />;
}

function HeadLine({ view, onRetry }: { view: HeadView; onRetry: () => void }) {
  return (
    <div className="tr-top" data-state={view.state}>
      <div className="tr-top__line">
        {view.lead !== "" && <span>{view.lead}</span>}
        {view.figure !== null && <b>{view.figure}</b>}
        {view.trail !== "" && <span>{view.trail}</span>}
        {view.retry && (
          <button type="button" onClick={onRetry}>
            retry
          </button>
        )}
      </div>
      {view.bar && (
        <div className="tr-top__bar">
          {/* undefined width lets the stylesheet drive it: full when held,
              indeterminate while fetching. An inline width would beat both. */}
          <i style={view.fill === null ? undefined : { width: view.fill }} />
        </div>
      )}
    </div>
  );
}

export interface HeadOfDocumentProps {
  readonly feed: TimelineFeed;
  /** The accumulator, 0..1, from `useHeadOfDocument`. */
  readonly pull: number;
  /** First recorded conversation activity, for the session-start line. */
  readonly startedAt: number | null;
  /** The clock an anchor search is paging toward, while one runs. */
  readonly finding?: string | null;
}

export function HeadOfDocument({ feed, pull, startedAt, finding }: HeadOfDocumentProps) {
  const view = describeHead({
    earlier: feed.earlier,
    above: feed.earlierAbove,
    pull,
    finding: finding ?? null,
    startLabel: sessionStartLabel(startedAt),
  });
  return (
    <>
      <HeadLine view={view} onRetry={feed.retryEarlier} />
      {feed.earlierNotice !== null && (
        // Losing the place is stated, not hidden. Muted, never red — nothing
        // was destroyed; the tail the reader is looking at is already current.
        <div className="tr-top" data-state="rewritten">
          <div className="tr-top__line">
            <span>{feed.earlierNotice.message}</span>
          </div>
        </div>
      )}
    </>
  );
}
