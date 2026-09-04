// Canvas cannot inherit the file viewer's CSS palette. Resolve the same custom
// properties through a live descendant, then let a one-pixel canvas normalize
// CSS Color 4 values and alpha-composite them over the viewer ground.

import type { TokenKind } from "../syntax";
import { packRgba } from "./raster";
import type { MinimapPalette } from "./raster";

const TOKEN_KINDS: readonly TokenKind[] = [
  "comment",
  "string",
  "number",
  "keyword",
  "type",
  "function",
  "constant",
  "property",
  "tag",
  "operator",
  "plain",
];

const TOKEN_VARIABLE: Readonly<Record<TokenKind, string>> = {
  comment: "--vf-text-faint",
  string: "--ah-syn-string",
  number: "--ah-syn-value",
  keyword: "--ah-syn-keyword",
  type: "--ah-syn-type",
  function: "--ah-syn-function",
  constant: "--ah-syn-value",
  property: "--ah-syn-property",
  tag: "--ah-syn-keyword",
  operator: "--vf-text-tertiary",
  plain: "--vf-text-secondary",
};

const BACKGROUND_VARIABLE = "--vf-chrome-solid";
const TOKEN_OPACITY = 0.58;

function packedPixel(
  context: CanvasRenderingContext2D,
  background: string,
  foreground: string,
  opacity: number,
): number {
  context.clearRect(0, 0, 1, 1);
  context.globalAlpha = 1;
  context.fillStyle = background;
  context.fillRect(0, 0, 1, 1);
  context.globalAlpha = opacity;
  context.fillStyle = foreground;
  context.fillRect(0, 0, 1, 1);
  context.globalAlpha = 1;
  const [red, green, blue, alpha] = context.getImageData(0, 0, 1, 1).data;
  return packRgba(red, green, blue, alpha);
}

export function resolveMinimapPalette(host: HTMLElement): MinimapPalette | null {
  const scratch = document.createElement("canvas");
  scratch.width = 1;
  scratch.height = 1;
  const context = scratch.getContext("2d", { willReadFrequently: true });
  if (context === null) return null;

  const probe = document.createElement("span");
  probe.style.position = "absolute";
  probe.style.visibility = "hidden";
  probe.style.pointerEvents = "none";
  host.appendChild(probe);

  try {
    const resolve = (variable: string): string => {
      probe.style.color = `var(${variable})`;
      return getComputedStyle(probe).color;
    };

    const backgroundCss = resolve(BACKGROUND_VARIABLE);
    const background = packedPixel(context, backgroundCss, backgroundCss, 1);
    const token = {} as Record<TokenKind, number>;
    for (const kind of TOKEN_KINDS) {
      token[kind] = packedPixel(
        context,
        backgroundCss,
        resolve(TOKEN_VARIABLE[kind]),
        TOKEN_OPACITY,
      );
    }
    return { background, token };
  } finally {
    probe.remove();
  }
}
