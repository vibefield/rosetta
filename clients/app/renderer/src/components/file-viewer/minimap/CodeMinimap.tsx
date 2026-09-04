// A read-only source map beside CodeView.
//
// Text is painted only on file/theme/size changes. Scroll moves one DOM
// thumb, derived from the native scroller's real geometry. Sampling changes
// which lines become pixels; it never changes navigation.

import { useLayoutEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import type { TokenLine } from "../syntax";
import {
  computeThumbGeometry,
  createRasterLayout,
  needsMinimap,
  scrollTopForMapOffset,
  scrollTopForThumbDelta,
  wheelDeltaPixels,
} from "./layout";
import type { MinimapThumbGeometry } from "./layout";
import { resolveMinimapPalette } from "./palette";
import { rasterizeMinimap } from "./raster";

const MAX_RENDER_SCALE = 2;

export interface CodeMinimapProps {
  readonly lines: readonly TokenLine[];
  readonly lineHeight: number;
  readonly tabColumns: number;
  readonly scrollerRef: RefObject<HTMLDivElement | null>;
}

interface DragState {
  readonly pointerId: number;
  readonly startY: number;
  readonly startScrollTop: number;
  readonly coordinateScale: number;
  readonly geometry: MinimapThumbGeometry;
}

function renderScale(): number {
  const ratio = Number.isFinite(window.devicePixelRatio) ? window.devicePixelRatio : 1;
  return Math.min(MAX_RENDER_SCALE, Math.max(1, ratio));
}

export function CodeMinimap({
  lines,
  lineHeight,
  tabColumns,
  scrollerRef,
}: CodeMinimapProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const thumbRef = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);
  const [supported, setSupported] = useState(true);

  // This effect remains alive when the minimap returns null. That is what
  // lets a newly overflowing scroller bring the host into existence without
  // requiring an already measurable canvas.
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (scroller === null || lines.length === 0 || !supported) {
      setVisible(false);
      return;
    }

    let frame = 0;
    const measure = (): void => {
      frame = 0;
      const next = needsMinimap(scroller.scrollHeight, scroller.clientHeight);
      setVisible((current) => (current === next ? current : next));
    };
    const schedule = (): void => {
      if (frame === 0) frame = requestAnimationFrame(measure);
    };

    measure();
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    observer?.observe(scroller);
    if (scroller.firstElementChild instanceof HTMLElement) {
      observer?.observe(scroller.firstElementChild);
    }
    window.addEventListener("resize", schedule);

    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", schedule);
      if (frame !== 0) cancelAnimationFrame(frame);
    };
  }, [lines.length, scrollerRef, supported]);

  useLayoutEffect(() => {
    if (!visible || !supported) return;

    const scroller = scrollerRef.current;
    const host = hostRef.current;
    const stage = stageRef.current;
    const canvas = canvasRef.current;
    const thumb = thumbRef.current;
    if (
      scroller === null ||
      host === null ||
      stage === null ||
      canvas === null ||
      thumb === null
    ) {
      return;
    }

    const context = canvas.getContext("2d", { alpha: false });
    const initialPalette = resolveMinimapPalette(host);
    if (context === null || initialPalette === null) {
      setSupported(false);
      return;
    }
    let palette = initialPalette;

    let trackHeight = 0;
    let geometry: MinimapThumbGeometry | null = null;
    let drag: DragState | null = null;
    let frame = 0;
    let paintPending = false;
    let palettePending = false;

    const syncThumb = (): void => {
      geometry = computeThumbGeometry({
        scrollTop: scroller.scrollTop,
        scrollHeight: scroller.scrollHeight,
        clientHeight: scroller.clientHeight,
        trackHeight,
      });
      thumb.style.height = `${geometry.height.toFixed(3)}px`;
      thumb.style.transform = `translate3d(0, ${geometry.top.toFixed(3)}px, 0)`;
    };

    const paint = (): void => {
      const scale = renderScale();
      const cssWidth = host.clientWidth;
      const availableHeight = host.clientHeight;
      const layout = createRasterLayout(lines.length, availableHeight, scale);
      if (cssWidth <= 0 || layout === null) {
        trackHeight = 0;
        stage.style.height = "0px";
        syncThumb();
        return;
      }

      const backingWidth = Math.max(1, Math.round(cssWidth * scale));
      if (canvas.width !== backingWidth) canvas.width = backingWidth;
      if (canvas.height !== layout.backingHeight) canvas.height = layout.backingHeight;
      trackHeight = layout.cssHeight;
      stage.style.height = `${trackHeight}px`;

      const image = context.createImageData(backingWidth, layout.backingHeight);
      rasterizeMinimap({
        pixels: new Uint32Array(image.data.buffer),
        width: backingWidth,
        height: layout.backingHeight,
        lines,
        lineIndexes: layout.lineIndexes,
        tabColumns,
        palette,
      });
      context.putImageData(image, 0, 0);
      syncThumb();
    };

    const flush = (): void => {
      frame = 0;
      if (palettePending) {
        palettePending = false;
        const resolved = resolveMinimapPalette(host);
        if (resolved === null) {
          setSupported(false);
          return;
        }
        palette = resolved;
      }
      if (paintPending) {
        paintPending = false;
        paint();
      } else {
        syncThumb();
      }
    };

    const schedule = (): void => {
      if (frame === 0) frame = requestAnimationFrame(flush);
    };
    const schedulePaint = (): void => {
      paintPending = true;
      schedule();
    };
    const schedulePalette = (): void => {
      palettePending = true;
      paintPending = true;
      schedule();
    };

    const onScroll = (): void => schedule();
    const onWheel = (event: WheelEvent): void => {
      const delta = wheelDeltaPixels(
        event.deltaY,
        event.deltaMode,
        lineHeight,
        scroller.clientHeight,
      );
      if (delta === 0) return;
      event.preventDefault();
      scroller.scrollTop += delta;
      schedule();
    };
    const onStagePointerDown = (event: PointerEvent): void => {
      if (event.button !== 0 || event.target === thumb) return;
      event.preventDefault();
      const bounds = stage.getBoundingClientRect();
      if (bounds.height <= 0 || trackHeight <= 0) return;
      const offset = (event.clientY - bounds.top) * (trackHeight / bounds.height);
      scroller.scrollTop = scrollTopForMapOffset({
        offset,
        trackHeight,
        scrollHeight: scroller.scrollHeight,
        clientHeight: scroller.clientHeight,
      });
      schedule();
    };
    const onThumbPointerDown = (event: PointerEvent): void => {
      if (event.button !== 0) return;
      event.preventDefault();
      event.stopPropagation();
      syncThumb();
      if (geometry === null) return;
      const bounds = stage.getBoundingClientRect();
      drag = {
        pointerId: event.pointerId,
        startY: event.clientY,
        startScrollTop: scroller.scrollTop,
        coordinateScale: bounds.height > 0 ? trackHeight / bounds.height : 1,
        geometry,
      };
      thumb.classList.add("is-dragging");
      thumb.setPointerCapture(event.pointerId);
    };
    const onThumbPointerMove = (event: PointerEvent): void => {
      if (drag === null || drag.pointerId !== event.pointerId) return;
      const delta = (event.clientY - drag.startY) * drag.coordinateScale;
      scroller.scrollTop = scrollTopForThumbDelta(
        drag.startScrollTop,
        delta,
        drag.geometry,
      );
      schedule();
    };
    const finishDrag = (event: PointerEvent, release: boolean): void => {
      if (drag === null || drag.pointerId !== event.pointerId) return;
      const pointerId = drag.pointerId;
      drag = null;
      thumb.classList.remove("is-dragging");
      if (release && thumb.hasPointerCapture(pointerId)) {
        thumb.releasePointerCapture(pointerId);
      }
      schedule();
    };
    const onThumbPointerUp = (event: PointerEvent): void => finishDrag(event, true);
    const onThumbLostCapture = (event: PointerEvent): void => finishDrag(event, false);

    scroller.addEventListener("scroll", onScroll, { passive: true });
    host.addEventListener("wheel", onWheel, { passive: false });
    stage.addEventListener("pointerdown", onStagePointerDown);
    thumb.addEventListener("pointerdown", onThumbPointerDown);
    thumb.addEventListener("pointermove", onThumbPointerMove);
    thumb.addEventListener("pointerup", onThumbPointerUp);
    thumb.addEventListener("pointercancel", onThumbPointerUp);
    thumb.addEventListener("lostpointercapture", onThumbLostCapture);

    const resizeObserver =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedulePaint);
    resizeObserver?.observe(host);
    resizeObserver?.observe(scroller);

    const themeObserver =
      typeof MutationObserver === "undefined"
        ? null
        : new MutationObserver(schedulePalette);
    themeObserver?.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class", "data-theme"],
    });

    let densityQuery: MediaQueryList | null = null;
    const onDensityChange = (): void => {
      armDensityObserver();
      schedulePaint();
    };
    const armDensityObserver = (): void => {
      densityQuery?.removeEventListener("change", onDensityChange);
      densityQuery = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
      densityQuery.addEventListener("change", onDensityChange);
    };
    armDensityObserver();
    window.addEventListener("resize", schedulePaint);

    paint();

    return () => {
      scroller.removeEventListener("scroll", onScroll);
      host.removeEventListener("wheel", onWheel);
      stage.removeEventListener("pointerdown", onStagePointerDown);
      thumb.removeEventListener("pointerdown", onThumbPointerDown);
      thumb.removeEventListener("pointermove", onThumbPointerMove);
      thumb.removeEventListener("pointerup", onThumbPointerUp);
      thumb.removeEventListener("pointercancel", onThumbPointerUp);
      thumb.removeEventListener("lostpointercapture", onThumbLostCapture);
      resizeObserver?.disconnect();
      themeObserver?.disconnect();
      densityQuery?.removeEventListener("change", onDensityChange);
      window.removeEventListener("resize", schedulePaint);
      if (frame !== 0) cancelAnimationFrame(frame);
    };
  }, [lineHeight, lines, scrollerRef, supported, tabColumns, visible]);

  if (!visible || !supported) return null;

  return (
    <div className="ah-code-minimap" ref={hostRef} aria-hidden="true" role="presentation">
      <div className="ah-code-minimap__stage" ref={stageRef}>
        <canvas className="ah-code-minimap__text" ref={canvasRef} />
        <div className="ah-code-minimap__slider" ref={thumbRef} />
      </div>
    </div>
  );
}
