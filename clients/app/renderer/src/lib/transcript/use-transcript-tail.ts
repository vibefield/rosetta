import { useCallback, useLayoutEffect, useRef, useState } from "react";
import type { TimelineEntry } from "@rosetta/protocol";
import { REDUCED_MOTION_QUERY, SETTLE_EASING } from "../motion.ts";
import { TailArrivals } from "./tail-state.ts";
import { TailScroll } from "./tail-scroll.ts";
import type { TailState } from "./tail-scroll.ts";

export interface TimelineLayout {
  readonly sessionId: string;
  readonly rows: readonly TimelineEntry[];
  readonly ready: boolean;
  readonly readySessionIds: readonly string[];
  readonly elementForRow: (row: TimelineEntry) => HTMLElement | null;
  /** Ownership/coalescing in the displayed chronology may differ from the source page. */
  readonly elementForAnchor?: (timelineId: string) => HTMLElement | null;
}

const EMPTY: TailState = { away: false, unseen: 0 };

export function useTranscriptTail(
  scrollerRef: { current: HTMLElement | null },
  sessionId: string | null,
  enabled: boolean,
  seeking: boolean,
) {
  const [state, setState] = useState<TailState>(EMPTY);
  const controller = useRef<TailScroll | null>(null);
  const layout = useRef<TimelineLayout | null>(null);
  const accept = useRef<((layout: TimelineLayout) => void) | null>(null);

  // The document owns the displayed chronology (including shared team
  // messages). Buffer its first child layout until the parent has mounted.
  const onLayout = useCallback((next: TimelineLayout) => {
    layout.current = next;
    accept.current?.(next);
  }, []);

  useLayoutEffect(() => {
    setState(EMPTY);
    const element = scrollerRef.current;
    if (!enabled || sessionId === null || element === null) return;
    const preference = window.matchMedia(REDUCED_MOTION_QUERY);
    const scroll = new TailScroll(element, setState, () => preference.matches);
    controller.current = scroll;
    if (seeking) scroll.pause();
    const arrivals = new TailArrivals();
    const animations = new Set<Animation>();
    const cancelEntrances = () => {
      for (const animation of animations) animation.cancel();
      animations.clear();
    };

    const update = (next: TimelineLayout) => {
      if (next.sessionId !== sessionId) return;
      if (!next.ready) {
        arrivals.reset();
        scroll.update(false, 0);
        return;
      }
      const fresh = arrivals.update(next.rows);
      for (const id of next.readySessionIds) arrivals.prime(id);
      scroll.update(true, fresh.length);
      if (fresh.length === 0 || preference.matches) return;

      const viewport = element.getBoundingClientRect();
      const allowance = scroll.isFollowing ? element.clientHeight : 0;
      // Animate actual new message elements, never an existing fold's
      // representative, a patch, a historical page, or a whole transcript.
      const entering = fresh.slice(-24).flatMap((row) => {
        const target = next.elementForRow(row);
        if (target === null || !element.contains(target)) return [];
        const box = target.getBoundingClientRect();
        return box.height > 0 &&
          box.bottom >= viewport.top &&
          box.top <= viewport.bottom + allowance
          ? [target]
          : [];
      });
      for (const target of entering) {
        const animation = target.animate(
          [
            { opacity: 0, translate: "0 6px" },
            { opacity: 1, translate: "0 0" },
          ],
          { duration: 280, easing: SETTLE_EASING },
        );
        animations.add(animation);
        animation.onfinish = () => animations.delete(animation);
      }
    };
    accept.current = update;
    if (layout.current) update(layout.current);

    const wheel = (event: WheelEvent) => {
      if (Math.abs(event.deltaY) > Math.abs(event.deltaX)) scroll.pause();
    };
    const pointer = (event: PointerEvent) => {
      // Native scrollbar drags and clicks on the scroll surface also express
      // reader intent, even on platforms that do not send wheel events.
      if (event.target === element || scroll.isAnimating) scroll.pause();
    };
    const focus = (event: FocusEvent) => {
      // Tabbing to a message action is reading/navigation intent. Keep that
      // focused control in view instead of continuing a return past it.
      if (event.target !== element) scroll.pause();
    };
    let touchY: number | null = null;
    const touchStart = (event: TouchEvent) => {
      touchY = event.touches[0]?.clientY ?? null;
    };
    const touchMove = (event: TouchEvent) => {
      if (touchY !== null && Math.abs((event.touches[0]?.clientY ?? touchY) - touchY) > 4) {
        scroll.pause();
      }
    };
    const key = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
      if (
        event.target instanceof Element &&
        event.target.closest("input, textarea, select, button, a, [contenteditable='true']")
      )
        return;
      if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(event.key)) {
        scroll.pause();
      }
    };
    const preferenceChanged = () => {
      if (preference.matches) cancelEntrances();
      scroll.resize();
    };
    // Observe the document as well as the viewport: images, disclosures,
    // streamed text and window resizing can change the foot without a row.
    const observer = new ResizeObserver(scroll.resize);
    observer.observe(element);
    const content = element.querySelector(".tr-col");
    if (content) observer.observe(content);
    element.addEventListener("scroll", scroll.onScroll, { passive: true });
    element.addEventListener("wheel", wheel, { passive: true });
    element.addEventListener("pointerdown", pointer, { passive: true });
    element.addEventListener("focusin", focus);
    element.addEventListener("touchstart", touchStart, { passive: true });
    element.addEventListener("touchmove", touchMove, { passive: true });
    element.addEventListener("keydown", key);
    preference.addEventListener("change", preferenceChanged);
    return () => {
      accept.current = null;
      controller.current = null;
      observer.disconnect();
      cancelEntrances();
      scroll.dispose();
      element.removeEventListener("scroll", scroll.onScroll);
      element.removeEventListener("wheel", wheel);
      element.removeEventListener("pointerdown", pointer);
      element.removeEventListener("focusin", focus);
      element.removeEventListener("touchstart", touchStart);
      element.removeEventListener("touchmove", touchMove);
      element.removeEventListener("keydown", key);
      preference.removeEventListener("change", preferenceChanged);
    };
    // Seeking changes pause the existing controller below; they must not
    // recreate it or forget unread arrivals in this session.
  }, [enabled, sessionId, scrollerRef]);

  useLayoutEffect(() => {
    if (seeking) controller.current?.pause();
  }, [seeking]);

  const pause = useCallback(() => controller.current?.pause(), []);
  const latest = useCallback(() => controller.current?.latest(), []);
  return { ...state, onLayout, pause, latest };
}
