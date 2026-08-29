// Markdown, parsed to a tree. See `types.ts` for why it is never a string.

import { parseBlocks } from "./blocks";
import type { Block } from "./types";

export type { Alignment, Block, Inline } from "./types";

/** Parse a whole document. Pure: no DOM, no network, no clock. */
export function parseMarkdown(text: string): readonly Block[] {
  // Tabs are four columns for indentation purposes, and a document that
  // mixes them with spaces should not lose its nesting over it.
  const lines = text.replace(/\r\n?/g, "\n").split("\n").map((line) => line.replace(/\t/g, "    "));
  return parseBlocks(lines);
}
