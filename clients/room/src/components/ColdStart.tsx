import { PAUSED_UPDATES } from "../lib/ingest-activity";
import type { IngestActivity } from "../lib/ingest-activity";
import { IngestStatus } from "./IngestStatus";
import type { HoldWords } from "./ui/Hold";

export function ColdStartPanel({ activity }: { readonly activity: IngestActivity | null }) {
  return (
    <div className="ah-empty ah-cold-start" data-face="cold-start">
      <strong>Bringing your history together</strong>
      <p>Your projects will appear here as we discover your conversations.</p>
      <IngestStatus activity={activity} />
    </div>
  );
}

/**
 * The cold start as a hold (R10): before the first project the room's own
 * words for the step stand under the host's mark — the step's title, with the
 * ellipsis of work under way, over its amount ("Discovering conversations…"
 * over "1,333 files checked"; "History import paused" over "80 sessions
 * found"). The first step's title stands alone: its amount ("Looking for
 * conversations…") counts nothing and says the same wait again. Paused updates
 * say the card's words, the mark at rest; a paused import its own title, at
 * rest too.
 */
export function coldStartHold(activity: IngestActivity | null): {
  readonly words: HoldWords;
  readonly resting: boolean;
} {
  if (activity === null) return { words: { key: "discovering", line: "Finding your history…" }, resting: false };
  const { phase, feed } = activity;
  if (feed.connectionError !== null) {
    return { words: { key: "paused-updates", line: PAUSED_UPDATES.title, sub: PAUSED_UPDATES.detail }, resting: true };
  }
  return {
    words: {
      key: phase.key,
      line: phase.active ? `${phase.title}…` : phase.title,
      sub: phase.key === "discovering" ? null : phase.amount,
    },
    resting: !phase.active,
  };
}
