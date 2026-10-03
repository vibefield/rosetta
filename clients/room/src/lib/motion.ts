/** Shared timing for small, interruptible movements in the reading surfaces. */
export const SETTLE_EASING = "cubic-bezier(0.22, 1, 0.36, 1)";
export const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

export function prefersReducedMotion(): boolean {
  return window.matchMedia(REDUCED_MOTION_QUERY).matches;
}

interface RowPosition {
  readonly top: number;
  readonly height: number;
}

/** FLIP in scroll-content coordinates, so ordinary scrolling never looks like
 * a reorder. Only rows crossing the viewport acquire animation layers. */
export class ListMotion {
  private positions = new Map<string, RowPosition>();
  private animations = new Map<string, { element: HTMLElement; animation: Animation }>();
  private initialized = false;

  update(container: HTMLElement, reduced: boolean): void {
    // Read the current visual displacement before cancelling an interrupted
    // transition. A second sort continues from the frame the reader sees.
    const offsets = new Map<string, { y: number; opacity: number }>();
    for (const [key, { element }] of this.animations) {
      const style = getComputedStyle(element);
      offsets.set(key, {
        y: style.transform === "none" ? 0 : new DOMMatrixReadOnly(style.transform).m42,
        opacity: Number(style.opacity),
      });
    }
    this.cancel();

    const edge = container.getBoundingClientRect().top;
    const top = container.scrollTop;
    const bottom = top + container.clientHeight;
    // Batch every layout read before starting any animations.
    const rows = Array.from(
      container.querySelectorAll<HTMLElement>("[data-motion-key]"),
      (element) => {
        const rect = element.getBoundingClientRect();
        return {
          element,
          key: element.dataset.motionKey!,
          position: { top: rect.top - edge + top, height: rect.height },
        };
      },
    );

    for (const { element, key, position } of rows) {
      const previous = this.positions.get(key);
      const oldTop =
        previous === undefined ? position.top : previous.top + (offsets.get(key)?.y ?? 0);
      const visibleAt = (y: number) => y + position.height >= top && y <= bottom;
      const crossesViewport = visibleAt(oldTop) || visibleAt(position.top);
      if (!this.initialized || reduced || !crossesViewport || position.height === 0) continue;

      // A row coming from thousands of entries away enters at the viewport
      // edge instead of racing across an entire archive in a single frame.
      const travel = container.clientHeight + position.height;
      const delta = Math.max(-travel, Math.min(travel, oldTop - position.top));
      const opacity = offsets.get(key)?.opacity ?? 1;
      const frames =
        previous === undefined
          ? [
              { opacity: 0, transform: "translateY(6px)" },
              { opacity: 1, transform: "none" },
            ]
          : Math.abs(delta) > 0.5 || opacity < 1
            ? [
                { transform: `translateY(${delta}px)`, opacity },
                { transform: "none", opacity: 1 },
              ]
            : null;
      if (frames === null) continue;
      const animation = element.animate(frames, {
        duration: previous === undefined ? 220 : 340,
        easing: SETTLE_EASING,
      });
      this.animations.set(key, { element, animation });
      animation.onfinish = () => {
        if (this.animations.get(key)?.animation === animation) this.animations.delete(key);
      };
    }
    this.positions = new Map(rows.map(({ key, position }) => [key, position]));
    this.initialized = true;
  }

  cancel(): void {
    for (const { animation } of this.animations.values()) animation.cancel();
    this.animations.clear();
  }
}
