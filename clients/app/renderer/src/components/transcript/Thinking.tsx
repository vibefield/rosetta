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
import { useEffect, useRef, useState } from "react";
import { firstLine, spanText } from "../../lib/transcript";
import { useFoldPreference } from "./fold-preference";
import { LineChevron, ThoughtGlyph } from "./glyphs";
import { useRowDisclosure } from "./disclosure-state";

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

/**
 * Q10 A (transcript-fold, 2026-09-08) — reasoning that stands alone between
 * two paragraphs is a run of one, and a run of one is the line it already was:
 * `Thought · its first words · the span`, on the ledger's rail, opening in
 * place to the same muted prose the clamp holds. Inside a ladder `Thinking`
 * keeps the clamp; this is the line form only. Q4 C moved one level down.
 */
export function ThoughtLine({
  text,
  spanMs,
  domId,
  timelineId,
  hit,
  anchor,
  forceOpen = false,
  embedded = false,
}: ThinkingProps & {
  /** A landing inside this thought opens it, as a landing inside a line does:
   * the matched words must be on the page, not one press away. */
  readonly forceOpen?: boolean;
  readonly embedded?: boolean;
}) {
  // §05 — the room's activity act is the default, a press is an override,
  // and the next press of the act clears it — as the run's head does.
  const everyRunOpen = !useFoldPreference().folded;
  const [override, setOverride] = useRowDisclosure(domId, !embedded && everyRunOpen);
  const actRef = useRef(everyRunOpen);
  useEffect(() => {
    if (actRef.current === everyRunOpen) return;
    actRef.current = everyRunOpen;
    if (!embedded) setOverride(everyRunOpen);
  }, [everyRunOpen, embedded, setOverride]);
  useEffect(() => {
    if (forceOpen || hit) setOverride(true);
  }, [forceOpen, hit]);
  const isOpen = override || forceOpen;
  const span = spanMs === null || spanMs < 1000 ? null : spanText(spanMs);
  // A thought may open with a blank line; the head names the first line
  // that says something.
  const lead = text.trimStart();
  const toggle = () => setOverride(!isOpen);
  return (
    <div
      className="tr-seg tr-ledger tr-thought"
      id={domId}
      data-timeline-id={timelineId}
      data-hit={hit || undefined}
    >
      {anchor}
      <div
        className="tr-line"
        role="button"
        tabIndex={0}
        aria-expanded={isOpen}
        data-thought=""
        onClick={toggle}
        onKeyDown={(event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          toggle();
        }}
      >
        <span className="tr-line__g">
          <ThoughtGlyph />
        </span>
        <span className="tr-line__v">Thought</span>
        <span className="tr-line__o" title={firstLine(lead, 400)}>
          {firstLine(lead)}
        </span>
        <span className="tr-line__r">
          {span !== null && <span>{span}</span>}
          <span className="chev">
            <LineChevron />
          </span>
        </span>
      </div>
      {isOpen && (
        <div className="tr-think" data-open="true">
          <div className="tr-think__text">
            {text.split(/\n{2,}/).map((paragraph, at) =>
              paragraph.trim() === "" ? null : <p key={at}>{paragraph}</p>,
            )}
          </div>
        </div>
      )}
    </div>
  );
}
