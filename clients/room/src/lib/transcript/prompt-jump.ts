// A button at the side goes to the message before — what a press decides, as
// pure functions over what the component reads from the layout: the person's
// own messages' tops and the view's top (the head's edge), both from the top
// of the scroller's frame, and the scroller's offset.
//
// A press goes to the nearest message of the person's own that begins above
// the top of the view and stands its top 12 under the head; pressed again it
// goes to the one before. More than a screen away it goes at once, nearer on
// the 300 ms ease, and never animated under Reduce Motion. The button is there
// only while such a message begins above, or rows above are not yet read; at
// the first message with rows above unread, a press goes to the document's
// top, where the rows above are read in as they are for a reader who scrolls
// there.
import type { EarlierState } from "./head-state";

/** Where a landed message's top stands: this far under the head. */
export const PROMPT_JUMP_LANDING_PX = 12;
/** The ease a near jump moves on. */
export const PROMPT_JUMP_EASE_MS = 300;
/** Half a device pixel: a top at the view's top is in the view, not above it. */
const AT_THE_TOP_PX = 0.5;

/**
 * The index of the nearest own message that begins above the view's top, or
 * null. `tops` are in document order; of two at one top, the earlier.
 */
export function previousMessageTarget(tops: readonly number[], viewTop: number): number | null {
  let target: number | null = null;
  for (let index = 0; index < tops.length; index += 1) {
    const top = tops[index]!;
    if (top >= viewTop - AT_THE_TOP_PX) continue;
    if (target === null || top > tops[target]!) target = index;
  }
  return target;
}

/** The scroll offset that stands a message's top 12 under the head, never above the document's top. */
export function jumpScrollTop(scrollTop: number, messageTop: number, viewTop: number): number {
  return Math.max(0, scrollTop + messageTop - viewTop - PROMPT_JUMP_LANDING_PX);
}

/** At once when more than a screen away or under Reduce Motion; else the 300 ms ease. */
export function jumpMotion(distance: number, viewHeight: number, reduced: boolean): "instant" | "ease" {
  return reduced || Math.abs(distance) > viewHeight ? "instant" : "ease";
}

/** Shown while an own message begins above the view, or rows above are not yet read. */
export function promptJumpShown(above: boolean, earlier: EarlierState["kind"]): boolean {
  return above || earlier !== "none";
}

export interface PromptJumpPlan {
  /** The message gone to, by its place in `tops`; null for the document's top. */
  readonly index: number | null;
  readonly scrollTop: number;
}

/** What a press does: a message above, else the document's top while rows above are unread, else nothing. */
export function promptJumpPlan(view: {
  readonly tops: readonly number[];
  readonly viewTop: number;
  readonly scrollTop: number;
  readonly earlier: EarlierState["kind"];
}): PromptJumpPlan | null {
  const index = previousMessageTarget(view.tops, view.viewTop);
  if (index !== null) {
    return { index, scrollTop: jumpScrollTop(view.scrollTop, view.tops[index]!, view.viewTop) };
  }
  return view.earlier === "none" ? null : { index: null, scrollTop: 0 };
}

// The room's settle curve (`SETTLE_EASING` in lib/motion.ts), as a function of
// time: a scroll offset cannot ride a CSS transition.
const X1 = 0.22;
const Y1 = 1;
const X2 = 0.36;
const Y2 = 1;
const bezier = (t: number, a: number, b: number): number =>
  3 * (1 - t) * (1 - t) * t * a + 3 * (1 - t) * t * t * b + t * t * t;

/** cubic-bezier(0.22, 1, 0.36, 1) at `x` in [0, 1], solved by bisection (monotone in x). */
export function settleEase(x: number): number {
  if (x <= 0) return 0;
  if (x >= 1) return 1;
  let low = 0;
  let high = 1;
  for (let step = 0; step < 32; step += 1) {
    const middle = (low + high) / 2;
    if (bezier(middle, X1, X2) < x) low = middle;
    else high = middle;
  }
  return bezier((low + high) / 2, Y1, Y2);
}
