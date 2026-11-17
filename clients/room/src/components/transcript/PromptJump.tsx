// A button at the side goes to the message before: a round glass button,
// level with the Latest pill and 16 px in from the transcript column's edge,
// holding a chevron up and no word; its name and tooltip are
// `Previous message`.
//
// A press goes to the nearest message of the person's own that begins above
// the top of the view and stands its top 12 under the head; pressed again, the
// one before. More than a screen away it goes at once, nearer on the 300 ms
// ease, never animated under Reduce Motion; the document stops following, as
// it does for any move up the page. Nothing else marks the message: its place
// at the top of the view is the mark. The button is there only while such a
// message begins above, or rows above are not yet read, and it fades over
// 200 ms; hidden, it takes no press. The decisions are `lib/transcript/
// prompt-jump.ts`; this reads the layout and moves the scroller.
import { useCallback, useEffect, useRef, useState } from "react";
import { prefersReducedMotion } from "../../lib/motion";
import {
  PROMPT_JUMP_EASE_MS,
  jumpMotion,
  previousMessageTarget,
  promptJumpPlan,
  promptJumpShown,
  settleEase,
} from "../../lib/transcript";
import type { EarlierState } from "../../lib/transcript/head-state";

/** The person's own messages: the prompt panel alone (a setting command wears a pill, a machine prompt an activity
 * row), and never one inside a worker's thread nested in the document. */
const OWN_MESSAGE = ".tr-prompt-message";
const NESTED = ".tr-sub";
/** Any of these from the reader ends a jump in flight: the reader has the page again. */
const READER_INPUT = ["wheel", "pointerdown", "touchstart", "keydown"] as const;

interface View {
  readonly tops: number[];
  readonly viewTop: number;
}

/**
 * The own messages' tops and the view's top, from the top of the scroller's frame. The head is the edge content
 * scrolls under: in a document that draws day pills, the band a pinned pill holds (its inset and its height), so a
 * landed message is never under the pill.
 */
function readView(scroller: HTMLElement): View {
  const frame = scroller.getBoundingClientRect();
  const day = scroller.querySelector<HTMLElement>(".tr-day");
  const pill = day?.querySelector<HTMLElement>(".tr-day__pill") ?? null;
  const viewTop =
    day === null || pill === null
      ? 0
      : (Number.parseFloat(getComputedStyle(day).top) || 0) + pill.getBoundingClientRect().height;
  const tops = Array.from(scroller.querySelectorAll<HTMLElement>(OWN_MESSAGE))
    .filter((element) => element.closest(NESTED) === null && element.getClientRects().length > 0)
    .map((element) => element.getBoundingClientRect().top - frame.top);
  return { tops, viewTop };
}

export interface PromptJumpProps {
  readonly scrollRef: { readonly current: HTMLElement | null };
  readonly earlier: EarlierState["kind"];
  /** Changes whenever the document's rows may have moved: what is above is read again. */
  readonly layoutKey: unknown;
  /** The document stops following, as it does for any move up the page. */
  readonly onLeaveTail: () => void;
  /** While the transcript is still opening, the button is not there. */
  readonly hidden: boolean;
}

export function PromptJump({ scrollRef, earlier, layoutKey, onLeaveTail, hidden }: PromptJumpProps) {
  const [above, setAbove] = useState(false);
  const inFlight = useRef<(() => void) | null>(null);

  // What is above, read once a frame on scroll, on a resize, and when the document changes.
  useEffect(() => {
    const scroller = scrollRef.current;
    if (scroller === null) return;
    let frame: number | null = null;
    const measure = (): void => {
      frame = null;
      const view = readView(scroller);
      setAbove(previousMessageTarget(view.tops, view.viewTop) !== null);
    };
    const schedule = (): void => {
      if (frame === null) frame = requestAnimationFrame(measure);
    };
    measure();
    scroller.addEventListener("scroll", schedule, { passive: true });
    const observer = new ResizeObserver(schedule);
    observer.observe(scroller);
    const content = scroller.querySelector(".tr-col");
    if (content !== null) observer.observe(content);
    return () => {
      if (frame !== null) cancelAnimationFrame(frame);
      scroller.removeEventListener("scroll", schedule);
      observer.disconnect();
    };
  }, [scrollRef, layoutKey]);

  useEffect(() => () => inFlight.current?.(), []);

  const press = useCallback(() => {
    const scroller = scrollRef.current;
    if (scroller === null) return;
    inFlight.current?.();
    const view = readView(scroller);
    const plan = promptJumpPlan({ ...view, scrollTop: scroller.scrollTop, earlier });
    // Keyboard navigation stays on the reading surface, as after the Latest pill.
    scroller.focus({ preventScroll: true });
    if (plan === null) return;
    onLeaveTail();
    const from = scroller.scrollTop;
    const distance = plan.scrollTop - from;
    if (jumpMotion(distance, scroller.clientHeight, prefersReducedMotion()) === "instant") {
      scroller.scrollTop = plan.scrollTop;
      return;
    }
    const start = performance.now();
    let frame = 0;
    const stop = (): void => {
      cancelAnimationFrame(frame);
      for (const type of READER_INPUT) scroller.removeEventListener(type, stop);
      inFlight.current = null;
    };
    const tick = (time: number): void => {
      const progress = Math.min(1, Math.max(0, (time - start) / PROMPT_JUMP_EASE_MS));
      scroller.scrollTop = from + distance * settleEase(progress);
      if (progress < 1) frame = requestAnimationFrame(tick);
      else stop();
    };
    for (const type of READER_INPUT) scroller.addEventListener(type, stop, { passive: true });
    inFlight.current = stop;
    frame = requestAnimationFrame(tick);
  }, [scrollRef, earlier, onLeaveTail]);

  const shown = !hidden && promptJumpShown(above, earlier);
  return (
    <button
      type="button"
      className="tr-prompt-jump"
      data-visible={shown || undefined}
      inert={!shown}
      aria-hidden={!shown || undefined}
      aria-label="Previous message"
      title="Previous message"
      onClick={press}
    >
      <svg width="15" height="15" viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <path
          d="M3.5 10.25 8 5.75l4.5 4.5"
          stroke="currentColor"
          strokeWidth="1.75"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    </button>
  );
}
