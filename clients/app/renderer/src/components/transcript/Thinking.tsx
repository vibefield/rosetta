// Q4 C (decided 2026-09-01) — reasoning is muted, clamped prose in the measure.
//
// Not a box and not a rail line: a Grok session writes paragraphs of it, and
// the shipped aside turns such a session back into a column of grey plates.
// Two lines of 13px tertiary prose, then `more · reasoning, 12s`, opening in
// place in the same register. The span comes from the adjacent stamps, which
// is the only honest source while openers are uncorrelated (§11).
//
// The segment owns `data-open` because the clamp is a CSS rule keyed on it,
// so the disclosure and the element the stylesheet reads are one component.
import type { ReactNode } from "react";
import { useState } from "react";
import { spanText } from "../../lib/transcript";

export interface ThinkingProps {
  readonly text: string;
  readonly spanMs: number | null;
  readonly domId: string;
  readonly timelineId: string;
  readonly hit: boolean;
  /** The margin anchor, drawn by the caller so every segment builds it once. */
  readonly anchor: ReactNode;
}

export function Thinking({ text, spanMs, domId, timelineId, hit, anchor }: ThinkingProps) {
  const [open, setOpen] = useState(false);
  const label =
    spanMs === null || spanMs < 1000
      ? "more · reasoning"
      : `more · reasoning, ${spanText(spanMs)}`;
  return (
    <div
      className="tr-seg tr-think"
      data-open={open}
      id={domId}
      data-timeline-id={timelineId}
      data-hit={hit || undefined}
    >
      {anchor}
      <div className="tr-think__text">
        {text.split(/\n{2,}/).map((paragraph, at) =>
          paragraph.trim() === "" ? null : <p key={at}>{paragraph}</p>,
        )}
      </div>
      <button
        type="button"
        className="tr-think__more"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        {open ? "less" : label}
      </button>
    </div>
  );
}
