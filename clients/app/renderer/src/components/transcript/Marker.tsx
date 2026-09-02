// §03 G — the marker family.
//
// The room already drew two markers: the live watermark and the data gap.
// The record makes them a FAMILY — every `Notice` is a hairline line with its
// words. Compaction, model change, mode change, permission, handoff. A runtime
// error takes red TEXT and nothing more: still a line, never a filled alarm.
//
// Four forms, four meanings, and only the day pill moves: notices are bare
// text on a hairline, the gap is dashed and pressable, the watermark is green.
// A permission decision is the marker BELOW the denied line, never a second
// ledger line — which needs no special case here, because the notice row
// already follows the tool row it decided about.

export interface MarkerProps {
  readonly text: string;
  /** A runtime error is the one marker that takes hue (T-5). */
  readonly tone?: "plain" | "failed";
  readonly domId?: string;
  readonly timelineId?: string;
  readonly hit?: boolean;
}

export function Marker({ text, tone = "plain", domId, timelineId, hit }: MarkerProps) {
  return (
    <div
      className={tone === "failed" ? "tr-mark tr-mark--error" : "tr-mark"}
      id={domId}
      data-timeline-id={timelineId}
      data-hit={hit || undefined}
    >
      {text}
    </div>
  );
}

/** D33's line, kept: rows arriving after you did start here. Green, because
 * this is the one marker that says something is alive. */
export function Watermark({ text, title }: { readonly text: string; readonly title?: string }) {
  return (
    <div className="tr-mark tr-mark--live" title={title}>
      {text}
    </div>
  );
}

/** A known unknown, and pressable because it should name its reason (§9).
 * The count is an ORDINAL discontinuity inside the loaded document — rows the
 * projection has, that this page does not. It is never drawn at the head,
 * where the head line already states what is above. */
export function GapMarker({
  count,
  onPress,
}: {
  readonly count: number;
  readonly onPress: () => void;
}) {
  return (
    <button type="button" className="tr-mark tr-mark--gap" onClick={onPress}>
      gap · {count} {count === 1 ? "message" : "messages"} not indexed
    </button>
  );
}
