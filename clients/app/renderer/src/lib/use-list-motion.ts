import { useLayoutEffect, useRef } from "react";
import { ListMotion, REDUCED_MOTION_QUERY } from "./motion.ts";

/** Call after rendering stable, sibling rows with data-motion-key identities.
 * `order` includes group labels so crossing a day boundary also moves rows. */
export function useListMotion(order: string, scope: string) {
  const ref = useRef<HTMLDivElement>(null);
  const refresh = useRef<(() => void) | null>(null);

  useLayoutEffect(() => {
    const container = ref.current;
    if (container === null) return;
    const instance = new ListMotion();
    const preference = window.matchMedia(REDUCED_MOTION_QUERY);
    const sizes = new Map<Element, { width: number; height: number }>();
    const observed = new Set<Element>();
    const resize = new ResizeObserver((entries) => {
      let changed = false;
      for (const { target, contentRect } of entries) {
        const previous = sizes.get(target);
        if (
          previous &&
          (Math.abs(previous.width - contentRect.width) > 0.5 ||
            Math.abs(previous.height - contentRect.height) > 0.5)
        )
          changed = true;
        sizes.set(target, { width: contentRect.width, height: contentRect.height });
      }
      // Progress labels, font metrics and window resizing can change row
      // heights without a reorder. Keep the next animation's origin current.
      if (changed) instance.update(container, preference.matches);
    });
    refresh.current = () => {
      const current = new Set<Element>([
        container,
        ...container.querySelectorAll("[data-motion-key]"),
      ]);
      for (const element of observed) {
        if (current.has(element)) continue;
        resize.unobserve(element);
        observed.delete(element);
        sizes.delete(element);
      }
      for (const element of current) {
        if (!observed.has(element)) resize.observe(element);
        observed.add(element);
      }
      instance.update(container, preference.matches);
    };
    const changed = () => {
      if (preference.matches) instance.cancel();
    };
    preference.addEventListener("change", changed);
    return () => {
      instance.cancel();
      resize.disconnect();
      preference.removeEventListener("change", changed);
      refresh.current = null;
    };
  }, [scope]);

  useLayoutEffect(() => {
    refresh.current?.();
  }, [order, scope]);

  return ref;
}
