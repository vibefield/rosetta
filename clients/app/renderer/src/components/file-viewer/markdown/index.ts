// Markdown, parsed to a tree. See `types.ts` for why it is never a string.

import { parseBlocks } from "./blocks";
import type { Block } from "./types";

export type { Alignment, Block, Inline } from "./types";

/** Parse a whole document. Pure: no DOM, no network, no clock. */
export function parseMarkdown(text: string, trackSource = false): readonly Block[] {
  if (!trackSource) {
    const lines = text.replace(/\r\n?/g, "\n").split("\n").map((line) => line.replace(/\t/g, "    "));
    return parseBlocks(lines);
  }
  // Maps use UTF-16 source offsets, matching JavaScript string slicing. They
  // are optional: ordinary files pay no per-character mapping cost.
  const lines: string[] = [];
  const sources: number[][] = [];
  let line = "";
  let positions: number[] = [];
  for (let i = 0; i < text.length; i += 1) {
    const char = text[i];
    if (char === "\r" || char === "\n") {
      lines.push(line); sources.push(positions); line = ""; positions = [];
      if (char === "\r" && text[i + 1] === "\n") i += 1;
    } else {
      const value = char === "\t" ? "    " : char;
      line += value;
      for (let j = 0; j < value!.length; j += 1) positions.push(i);
    }
  }
  lines.push(line); sources.push(positions);
  return parseBlocks(lines, sources);
}
