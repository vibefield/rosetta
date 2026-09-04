// The minimap's one hot pass: syntax-coloured blocks written directly into
// a pane-sized pixel buffer. It deliberately knows nothing about Canvas or
// the DOM, so node:test can exercise the exact renderer without a browser.

import type { TokenKind, TokenLine } from "../syntax";
import { MINIMAP_ROW_DEVICE_PIXELS } from "./layout.ts";

export const MINIMAP_CHARACTER_DEVICE_PIXELS = 1;
export const MINIMAP_CHARACTER_HEIGHT_DEVICE_PIXELS = 2;
export const MINIMAP_LEFT_GUTTER_DEVICE_PIXELS = 2;
export const MINIMAP_MAX_COLUMNS = 96;

export interface MinimapPalette {
  readonly background: number;
  readonly token: Readonly<Record<TokenKind, number>>;
}

const endianProbe = new Uint32Array([0x01020304]);
const LITTLE_ENDIAN = new Uint8Array(endianProbe.buffer)[0] === 0x04;
const COMBINING_MARK = /\p{Mark}/u;

export function packRgba(red: number, green: number, blue: number, alpha = 255): number {
  const r = red & 0xff;
  const g = green & 0xff;
  const b = blue & 0xff;
  const a = alpha & 0xff;
  return LITTLE_ENDIAN
    ? ((a << 24) | (b << 16) | (g << 8) | r) >>> 0
    : ((r << 24) | (g << 16) | (b << 8) | a) >>> 0;
}

/** A compact East Asian Width predicate. Unlike `codePoint > 0xff`, this
 * leaves Greek, Cyrillic, Hebrew, combining marks, and ordinary symbols at
 * their actual monospace width. */
function isFullWidthCodePoint(point: number): boolean {
  if (point < 0x1100) return false;
  return (
    point <= 0x115f ||
    point === 0x2329 ||
    point === 0x232a ||
    (point >= 0x2e80 && point <= 0x303e) ||
    (point >= 0x3040 && point <= 0xa4cf) ||
    (point >= 0xac00 && point <= 0xd7a3) ||
    (point >= 0xf900 && point <= 0xfaff) ||
    (point >= 0xfe10 && point <= 0xfe19) ||
    (point >= 0xfe30 && point <= 0xfe6f) ||
    (point >= 0xff00 && point <= 0xff60) ||
    (point >= 0xffe0 && point <= 0xffe6) ||
    (point >= 0x1b000 && point <= 0x1b001) ||
    (point >= 0x1f200 && point <= 0x1f251) ||
    (point >= 0x20000 && point <= 0x3fffd)
  );
}

function characterColumns(character: string): number {
  const point = character.codePointAt(0) ?? 0;
  if (point === 0x200d || COMBINING_MARK.test(character)) return 0;
  return isFullWidthCodePoint(point) ? 2 : 1;
}

function isGap(character: string): boolean {
  return (
    character === " " ||
    character === "\t" ||
    character === "\r" ||
    character === "\n" ||
    character === "\u00a0"
  );
}

function paintRun(
  pixels: Uint32Array,
  width: number,
  height: number,
  xFrom: number,
  xTo: number,
  y: number,
  color: number,
): void {
  const from = Math.max(0, Math.min(width, xFrom));
  const to = Math.max(from, Math.min(width, xTo));
  if (from === to) return;

  const bottom = Math.min(height, y + MINIMAP_CHARACTER_HEIGHT_DEVICE_PIXELS);
  for (let pixelY = y; pixelY < bottom; pixelY += 1) {
    pixels.fill(color, pixelY * width + from, pixelY * width + to);
  }
}

function rasterLine(
  pixels: Uint32Array,
  width: number,
  height: number,
  y: number,
  line: TokenLine,
  tabColumns: number,
  palette: MinimapPalette,
): void {
  const maxX = Math.min(
    width,
    MINIMAP_LEFT_GUTTER_DEVICE_PIXELS +
      MINIMAP_MAX_COLUMNS * MINIMAP_CHARACTER_DEVICE_PIXELS,
  );
  const tabs = Math.max(1, Math.floor(tabColumns));
  let column = 0;
  let x = MINIMAP_LEFT_GUTTER_DEVICE_PIXELS;

  for (const token of line) {
    if (x >= maxX) return;
    let runStart = -1;

    const flush = (): void => {
      if (runStart === -1) return;
      paintRun(pixels, width, height, runStart, Math.min(x, maxX), y, palette.token[token.kind]);
      runStart = -1;
    };

    for (const character of token.text) {
      if (isGap(character)) {
        flush();
        const advance =
          character === "\t" ? tabs - (column % tabs) : characterColumns(character);
        column += advance;
        x += advance * MINIMAP_CHARACTER_DEVICE_PIXELS;
        if (x >= maxX) return;
        continue;
      }

      const advance = characterColumns(character);
      if (advance === 0) continue;
      if (runStart === -1) runStart = x;
      column += advance;
      x += advance * MINIMAP_CHARACTER_DEVICE_PIXELS;
      if (x >= maxX) {
        flush();
        return;
      }
    }
    flush();
  }
}

export function rasterizeMinimap(args: {
  readonly pixels: Uint32Array;
  readonly width: number;
  readonly height: number;
  readonly lines: readonly TokenLine[];
  readonly lineIndexes: Uint32Array | null;
  readonly tabColumns: number;
  readonly palette: MinimapPalette;
}): void {
  const width = Math.max(0, Math.floor(args.width));
  const height = Math.max(0, Math.floor(args.height));
  const pixelCount = width * height;
  if (width === 0 || height === 0 || args.pixels.length < pixelCount) return;

  args.pixels.fill(args.palette.background, 0, pixelCount);
  const availableRows = Math.floor(height / MINIMAP_ROW_DEVICE_PIXELS);
  const rowCount = Math.min(
    availableRows,
    args.lineIndexes === null ? args.lines.length : args.lineIndexes.length,
  );

  for (let row = 0; row < rowCount; row += 1) {
    const lineIndex = args.lineIndexes === null ? row : args.lineIndexes[row];
    const line = args.lines[lineIndex];
    if (line === undefined) continue;
    rasterLine(
      args.pixels,
      width,
      height,
      row * MINIMAP_ROW_DEVICE_PIXELS,
      line,
      args.tabColumns,
      args.palette,
    );
  }
}
