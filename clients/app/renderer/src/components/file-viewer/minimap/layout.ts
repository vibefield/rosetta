// Geometry for a full-file, pane-bounded minimap.
//
// Sampling belongs only to the raster. Navigation is a continuous projection
// of the real scroll element, which keeps the thumb and pointer correct even
// when no representative sampled line happens to be inside the viewport.

export const MINIMAP_ROW_DEVICE_PIXELS = 2;
export const MINIMAP_MIN_THUMB_CSS_PIXELS = 8;

const OVERFLOW_EPSILON = 1;

export interface MinimapRasterLayout {
  readonly rowCount: number;
  readonly backingHeight: number;
  readonly cssHeight: number;
  /** Zero-based source line for each painted row. `null` means identity. */
  readonly lineIndexes: Uint32Array | null;
}

export interface MinimapThumbGeometry {
  readonly top: number;
  readonly height: number;
  readonly maxTop: number;
  readonly maxScroll: number;
  readonly scrollPerTrackPixel: number;
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, value));
}

function positive(value: number): number {
  return Number.isFinite(value) ? Math.max(0, value) : 0;
}

/** The browser's scroll box is authoritative: it includes code padding and
 * any horizontal scrollbar that reduced the vertical viewport. */
export function needsMinimap(scrollHeight: number, clientHeight: number): boolean {
  const viewport = positive(clientHeight);
  return viewport > 0 && positive(scrollHeight) - viewport > OVERFLOW_EPSILON;
}

/** One representative source line per equal source interval. Endpoints are
 * navigation concerns, not raster concerns; midpoint sampling avoids biasing
 * the picture toward the first and last line of every interval. */
export function sampleLineIndexes(lineCount: number, rowCount: number): Uint32Array | null {
  const lines = Math.max(0, Math.floor(lineCount));
  const rows = Math.max(0, Math.floor(rowCount));
  if (lines === 0 || rows === 0 || lines <= rows) return null;

  const sampled = new Uint32Array(rows);
  for (let row = 0; row < rows; row += 1) {
    const from = Math.floor((row * lines) / rows);
    const to = Math.max(from, Math.floor(((row + 1) * lines) / rows) - 1);
    sampled[row] = Math.floor((from + to) / 2);
  }
  return sampled;
}

export function createRasterLayout(
  lineCount: number,
  availableCssHeight: number,
  renderScale: number,
): MinimapRasterLayout | null {
  const lines = Math.max(0, Math.floor(lineCount));
  const cssHeight = positive(availableCssHeight);
  const scale = positive(renderScale);
  if (lines === 0 || cssHeight === 0 || scale === 0) return null;

  const availableDeviceHeight = Math.floor(cssHeight * scale);
  const capacity = Math.floor(availableDeviceHeight / MINIMAP_ROW_DEVICE_PIXELS);
  if (capacity === 0) return null;

  const rowCount = Math.min(lines, capacity);
  const backingHeight = rowCount * MINIMAP_ROW_DEVICE_PIXELS;
  return {
    rowCount,
    backingHeight,
    cssHeight: backingHeight / scale,
    lineIndexes: sampleLineIndexes(lines, rowCount),
  };
}

/** A scrollbar thumb over the part of the minimap that actually contains
 * pixels. For an unsampled 100-line file that is a short track at the top;
 * for a sampled file it is the full available pane height. */
export function computeThumbGeometry(args: {
  readonly scrollTop: number;
  readonly scrollHeight: number;
  readonly clientHeight: number;
  readonly trackHeight: number;
  readonly minimumHeight?: number;
}): MinimapThumbGeometry {
  const trackHeight = positive(args.trackHeight);
  const scrollHeight = positive(args.scrollHeight);
  const clientHeight = positive(args.clientHeight);
  const maxScroll = Math.max(0, scrollHeight - clientHeight);

  if (trackHeight === 0 || scrollHeight === 0 || maxScroll === 0) {
    return {
      top: 0,
      height: trackHeight,
      maxTop: 0,
      maxScroll,
      scrollPerTrackPixel: 0,
    };
  }

  const minimumHeight = positive(args.minimumHeight ?? MINIMAP_MIN_THUMB_CSS_PIXELS);
  const height = clamp((clientHeight / scrollHeight) * trackHeight, minimumHeight, trackHeight);
  const maxTop = Math.max(0, trackHeight - height);
  const top =
    maxTop === 0 ? 0 : (clamp(positive(args.scrollTop), 0, maxScroll) / maxScroll) * maxTop;

  return {
    top,
    height,
    maxTop,
    maxScroll,
    scrollPerTrackPixel: maxTop === 0 ? 0 : maxScroll / maxTop,
  };
}

/** Warp so the source position under the pointer lands in the viewport's
 * centre. The clamps make the first and last source positions reachable. */
export function scrollTopForMapOffset(args: {
  readonly offset: number;
  readonly trackHeight: number;
  readonly scrollHeight: number;
  readonly clientHeight: number;
}): number {
  const trackHeight = positive(args.trackHeight);
  const scrollHeight = positive(args.scrollHeight);
  const clientHeight = positive(args.clientHeight);
  const maxScroll = Math.max(0, scrollHeight - clientHeight);
  if (trackHeight === 0 || maxScroll === 0) return 0;

  const fraction = clamp(positive(args.offset) / trackHeight, 0, 1);
  return clamp(fraction * scrollHeight - clientHeight / 2, 0, maxScroll);
}

export function scrollTopForThumbDelta(
  startScrollTop: number,
  delta: number,
  geometry: MinimapThumbGeometry,
): number {
  return clamp(
    positive(startScrollTop) + delta * geometry.scrollPerTrackPixel,
    0,
    geometry.maxScroll,
  );
}

/** WheelEvent delta units: pixels, lines, or pages. */
export function wheelDeltaPixels(
  delta: number,
  deltaMode: number,
  lineHeight: number,
  clientHeight: number,
): number {
  if (!Number.isFinite(delta)) return 0;
  if (deltaMode === 1) return delta * positive(lineHeight);
  if (deltaMode === 2) return delta * positive(clientHeight);
  return delta;
}
