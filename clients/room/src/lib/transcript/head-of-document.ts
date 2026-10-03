// §08 — the head of the document, wired to a scroller.
//
// Four jobs, all of them listeners over `head-state.ts`'s arithmetic:
//   approach — within one viewport of the top, fetch the page above and HOLD it
//   the pull  — 120px of upward travel at scrollTop 0 spends the bar
//   landing   — the page lands above the viewport, which does not move
//   the fade  — `--frost` rises 0 → 1 over the first 24px of scroll
//
// CONTRACT WITH THE ROW COMPONENTS: every top-level row/turn element inside the
// scroller carries `data-timeline-id="<timeline_id>"`. Landing finds the first
// old row by that attribute, measures it before and after the prepend, and adds
// the difference to scrollTop — Chromium suppresses scroll anchoring at scroll
// position zero, which is precisely where this fires. Without the attribute the
// hook falls back to a scrollHeight delta, which is correct but coarser.
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { TimelineFeed } from "../hooks";
import type { PullModel } from "./head-state";
import {
  advancePull,
  frostOf,
  FROST_RANGE_PX,
  initialPull,
  PULL_DECAY_MS,
  PULL_THRESHOLD_PX,
  pullProgress,
  RECEIPT_MS,
} from "./head-state";

export interface HeadOfDocumentOptions {
  /** Upward travel that spends the bar. Default 120px. */
  readonly threshold?: number;
  /** Stillness that decays the accumulator. Default 500ms. */
  readonly decayMs?: number;
  /** The scroll depth `--frost` reaches 1 at. Default 24px. */
  readonly frostRange?: number;
  /** How long the receipt pill stands. Default 1800ms, the record's clock. */
  readonly receiptMs?: number;
  /** Set false to leave the listeners unbound (a search view, an empty room). */
  readonly enabled?: boolean;
}

export interface HeadOfDocument {
  /** The accumulator as the bar reads it, 0..1. */
  readonly pull: number;
  /** `--frost`, 0..1 — also written onto the scroller for the CSS to consume. */
  readonly frost: number;
  /** The last landing while its receipt stands, then null. */
  readonly receipt: { readonly count: number; readonly seq: number } | null;
}

/** The first row under the reader's eye, and where it sits, as of the last frame. */
interface Anchor {
  readonly id: string | null;
  /** Its viewport top. The landing holds this number still. */
  readonly top: number;
  /** The fallback measure, for a document whose rows carry no id. */
  readonly scrollHeight: number;
}

/** A box the reader can see. A line folded away inside a closed run, or an
 *  element not laid out, measures 0×0 at the viewport's origin — anchoring on
 *  it would hold nothing still and land the reader at the top instead. */
function visibleBox(element: HTMLElement): DOMRect | null {
  const box = element.getBoundingClientRect();
  if (box.height > 0 || box.width > 0) return box;
  // A closed activity run is still the visible location of its events. After
  // an earlier page joins it, anchor to that same run instead of the next chat.
  const group = element.closest(".tr-activity__list[hidden]")?.parentElement;
  if (group !== null && group !== undefined) {
    const groupBox = group.getBoundingClientRect();
    if (groupBox.height > 0) return groupBox;
  }
  return null;
}

/** Where the row with this id is drawn now, skipping any hidden twin: one
 *  timeline id may be stamped on a turn wrapper AND on the line inside it, and
 *  after a prepend the turns re-split, so the first match in DOM order can be
 *  the folded one. */
function rowTop(scroller: HTMLElement, id: string): number | null {
  const twins = scroller.querySelectorAll<HTMLElement>(
    `[data-timeline-id="${CSS.escape(id)}"]`,
  );
  for (const twin of twins) {
    const box = visibleBox(twin);
    if (box !== null) return box.top;
  }
  return null;
}

function measureAnchor(scroller: HTMLElement): Anchor {
  const edge = scroller.getBoundingClientRect().top;
  let id: string | null = null;
  let top = 0;
  // The first VISIBLE row whose bottom has not scrolled past the top edge:
  // that is the one under the reader's eye, whatever the DOM order says.
  for (const row of scroller.querySelectorAll<HTMLElement>("[data-timeline-id]")) {
    const box = visibleBox(row);
    if (box === null || box.bottom < edge) continue;
    id = row.dataset.timelineId ?? null;
    top = box.top;
    break;
  }
  return { id, top, scrollHeight: scroller.scrollHeight };
}

export function useHeadOfDocument(
  scrollerRef: { current: HTMLElement | null },
  feed: TimelineFeed,
  options: HeadOfDocumentOptions = {},
): HeadOfDocument {
  const threshold = options.threshold ?? PULL_THRESHOLD_PX;
  const decayMs = options.decayMs ?? PULL_DECAY_MS;
  const frostRange = options.frostRange ?? FROST_RANGE_PX;
  const receiptMs = options.receiptMs ?? RECEIPT_MS;
  const enabled = options.enabled ?? true;

  const [pull, setPull] = useState(0);
  const [frost, setFrost] = useState(0);
  const [receipt, setReceipt] = useState<HeadOfDocument["receipt"]>(null);

  const pullRef = useRef<PullModel>(initialPull);
  const anchorRef = useRef<Anchor | null>(null);
  const frameRef = useRef<number | null>(null);
  const decayRef = useRef<number | null>(null);

  // The listeners fire between commits; they must never close over a stale
  // feed. One ref, refreshed every render, is the whole trick.
  const feedRef = useRef(feed);
  feedRef.current = feed;

  const setPullModel = useCallback(
    (next: PullModel) => {
      pullRef.current = next;
      setPull(pullProgress(next, threshold));
    },
    [threshold],
  );

  // ── the landing ───────────────────────────────────────────────────────────
  // Declared FIRST so it reads the anchor cached by the commit BEFORE the
  // prepend; the refresher below then overwrites it for the next landing.
  const landSeq = feed.landed?.seq ?? null;
  useLayoutEffect(() => {
    if (landSeq === null) return;
    const scroller = scrollerRef.current;
    const before = anchorRef.current;
    if (scroller === null || before === null) return;
    const top = before.id === null ? null : rowTop(scroller, before.id);
    // The row that was under the reader's eye stays exactly where it was.
    // Chromium suppresses scroll anchoring at scroll position zero — precisely
    // where this fires — so the prepend adds the difference itself.
    const delta =
      top !== null ? top - before.top : scroller.scrollHeight - before.scrollHeight;
    if (delta !== 0) scroller.scrollTop += delta;
    anchorRef.current = measureAnchor(scroller);
  }, [landSeq, scrollerRef]);

  // Keep the anchor fresh: every commit, and every scroll frame below.
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (scroller !== null) anchorRef.current = measureAnchor(scroller);
  });

  // ── the receipt ───────────────────────────────────────────────────────────
  const landed = feed.landed;
  useEffect(() => {
    if (landed === null || landed.count <= 0) {
      // A session change drops the landing; a stale receipt must go with it.
      setReceipt(null);
      return;
    }
    setReceipt({ count: landed.count, seq: landed.seq });
    const timer = window.setTimeout(() => setReceipt(null), receiptMs);
    return () => window.clearTimeout(timer);
  }, [landed, receiptMs]);

  // ── the listeners ─────────────────────────────────────────────────────────
  useEffect(() => {
    const scroller = scrollerRef.current;
    if (scroller === null || !enabled) return;

    const clearDecay = () => {
      if (decayRef.current !== null) window.clearTimeout(decayRef.current);
      decayRef.current = null;
    };
    const armDecay = () => {
      clearDecay();
      decayRef.current = window.setTimeout(() => {
        decayRef.current = null;
        setPullModel(initialPull);
      }, decayMs);
    };

    /** The bar is spent. A held page lands now; a fetch in flight lands on
     *  arrival; and a pull that outran the approach starts the fetch itself. */
    const spendBar = () => {
      clearDecay();
      setPullModel(initialPull);
      feedRef.current.holdEarlier();
      feedRef.current.revealEarlier();
    };

    /** Upward travel at the top: bank it, and fire at the threshold. */
    const travel = (px: number) => {
      const kind = feedRef.current.earlier.kind;
      // A pull at the start does nothing, and the frame does not bounce.
      if (kind === "none") return;
      const result = advancePull(
        pullRef.current,
        { travel: px, atTop: scroller.scrollTop <= 0, now: Date.now() },
        { threshold, decayMs },
      );
      setPullModel(result.model);
      if (result.fire) {
        spendBar();
      } else if (result.model.travel > 0) {
        armDecay();
      } else {
        clearDecay();
      }
    };

    const readScroll = () => {
      frameRef.current = null;
      const next = frostOf(scroller.scrollTop, frostRange);
      scroller.style.setProperty("--frost", String(next));
      setFrost(next);
      anchorRef.current = measureAnchor(scroller);
      if (scroller.scrollTop > 0 && pullRef.current.travel > 0) {
        clearDecay();
        setPullModel(initialPull);
      }
      // Within one viewport of the top the next page is requested and held.
      // Nothing is prepended on approach.
      if (
        scroller.scrollTop <= scroller.clientHeight &&
        feedRef.current.earlier.kind === "idle"
      ) {
        feedRef.current.holdEarlier();
      }
    };

    const onScroll = () => {
      if (frameRef.current !== null) return;
      frameRef.current = window.requestAnimationFrame(readScroll);
    };

    const onWheel = (event: WheelEvent) => {
      if (scroller.scrollTop > 0) {
        if (pullRef.current.travel > 0) {
          clearDecay();
          setPullModel(initialPull);
        }
        return;
      }
      if (event.deltaY < 0) {
        // Not passive: the frame contains its own overscroll, so the window
        // never rubber-bands while the reader spends the bar (§08).
        event.preventDefault();
        travel(-event.deltaY);
      } else if (pullRef.current.travel > 0) {
        clearDecay();
        setPullModel(initialPull);
      }
    };

    let touchY: number | null = null;
    const onTouchStart = (event: TouchEvent) => {
      touchY = event.touches[0]?.clientY ?? null;
    };
    const onTouchMove = (event: TouchEvent) => {
      const y = event.touches[0]?.clientY;
      if (touchY === null || y === undefined || scroller.scrollTop > 0) return;
      const dy = y - touchY;
      touchY = y;
      if (dy > 0) travel(dy);
      else if (dy < 0 && pullRef.current.travel > 0) {
        clearDecay();
        setPullModel(initialPull);
      }
    };
    const onTouchEnd = () => {
      touchY = null;
    };

    // Home, PageUp and ↑ at the top reveal on one press (§08).
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Home" && event.key !== "PageUp" && event.key !== "ArrowUp") return;
      if (scroller.scrollTop > 0) return;
      const kind = feedRef.current.earlier.kind;
      if (kind === "none" || kind === "revealing") return;
      event.preventDefault();
      spendBar();
    };

    scroller.addEventListener("scroll", onScroll, { passive: true });
    scroller.addEventListener("wheel", onWheel, { passive: false });
    scroller.addEventListener("touchstart", onTouchStart, { passive: true });
    scroller.addEventListener("touchmove", onTouchMove, { passive: true });
    scroller.addEventListener("touchend", onTouchEnd, { passive: true });
    scroller.addEventListener("touchcancel", onTouchEnd, { passive: true });
    scroller.addEventListener("keydown", onKeyDown);
    readScroll();

    return () => {
      scroller.removeEventListener("scroll", onScroll);
      scroller.removeEventListener("wheel", onWheel);
      scroller.removeEventListener("touchstart", onTouchStart);
      scroller.removeEventListener("touchmove", onTouchMove);
      scroller.removeEventListener("touchend", onTouchEnd);
      scroller.removeEventListener("touchcancel", onTouchEnd);
      scroller.removeEventListener("keydown", onKeyDown);
      if (frameRef.current !== null) window.cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
      clearDecay();
      scroller.style.removeProperty("--frost");
    };
  }, [scrollerRef, enabled, threshold, decayMs, frostRange, setPullModel]);

  // After a landing the reader may still be within a viewport of the top: the
  // record re-approaches immediately so the next page is already held.
  const earlierKind = feed.earlier.kind;
  useEffect(() => {
    if (!enabled || earlierKind !== "idle") return;
    const scroller = scrollerRef.current;
    if (scroller === null) return;
    if (scroller.scrollTop <= scroller.clientHeight) feedRef.current.holdEarlier();
  }, [enabled, earlierKind, landSeq, scrollerRef]);

  return { pull, frost, receipt };
}
