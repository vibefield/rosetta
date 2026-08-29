// Block markdown: the structure a document is made of, read line by line.
//
// The subset is the one real documents are written in — headings, fences,
// lists, quotes, rules, GFM tables, paragraphs. What is deliberately absent
// is raw HTML: an HTML block is kept as the text it is, because rendering it
// would be the one thing `types.ts` says this parser must never do.

import { parseInline, parseInlineLines } from "./inline";
import type { Alignment, Block, Inline } from "./types";

const ATX_HEADING = /^ {0,3}(#{1,6})(?:\s+(.*?))?\s*(?:\s#+)?$/;
const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^`\s]*)/;
const RULE = /^ {0,3}([-*_])[ \t]*(?:\1[ \t]*){2,}$/;
const QUOTE = /^ {0,3}> ?/;
const BULLET = /^(\s*)([-*+]|\d{1,9}[.)])(\s+|$)/;
const SETEXT = /^ {0,3}(=+|-+)\s*$/;
const TABLE_DELIMITER = /^\s*\|?(?:\s*:?-+:?\s*\|)+\s*:?-*:?\s*\|?\s*$/;

const isBlank = (line: string): boolean => line.trim() === "";
const indentOf = (line: string): number => line.length - line.trimStart().length;

/** Does a line interrupt the paragraph above it? */
function startsBlock(line: string): boolean {
  return (
    ATX_HEADING.test(line) ||
    FENCE.test(line) ||
    RULE.test(line) ||
    QUOTE.test(line) ||
    BULLET.test(line)
  );
}

interface Bullet {
  readonly ordered: boolean;
  readonly start: number;
  readonly indent: number;
  /** Where the item's own content begins, so nested blocks can be dedented. */
  readonly contentIndent: number;
  readonly content: string;
}

function bulletOf(line: string): Bullet | null {
  const found = BULLET.exec(line);
  // `- - -` is a rule wearing a bullet's clothes, and the rule reading wins.
  if (found === null || RULE.test(line)) return null;
  const [whole, lead, marker] = found;
  return {
    ordered: /\d/.test(marker),
    start: /\d/.test(marker) ? Number.parseInt(marker, 10) : 1,
    indent: lead.length,
    contentIndent: whole.length,
    content: line.slice(whole.length),
  };
}

function splitRow(line: string): readonly string[] {
  const cells: string[] = [];
  let cell = "";
  const body = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  for (let i = 0; i < body.length; i += 1) {
    if (body[i] === "\\" && body[i + 1] === "|") {
      cell += "|";
      i += 1;
      continue;
    }
    if (body[i] === "|") {
      cells.push(cell.trim());
      cell = "";
      continue;
    }
    cell += body[i];
  }
  cells.push(cell.trim());
  return cells;
}

function alignmentOf(cell: string): Alignment {
  const left = cell.startsWith(":");
  const right = cell.endsWith(":");
  if (left && right) return "center";
  if (right) return "right";
  if (left) return "left";
  return null;
}

const inlineCells = (line: string): readonly (readonly Inline[])[] =>
  splitRow(line).map((cell) => parseInline(cell));

export function parseBlocks(lines: readonly string[]): readonly Block[] {
  const out: Block[] = [];
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];

    if (isBlank(line)) {
      index += 1;
      continue;
    }

    const fence = FENCE.exec(line);
    if (fence !== null) {
      const marker = fence[1][0];
      const body: string[] = [];
      index += 1;
      while (index < lines.length) {
        const closing = FENCE.exec(lines[index]);
        if (closing !== null && closing[1][0] === marker && closing[1].length >= fence[1].length) {
          index += 1;
          break;
        }
        body.push(lines[index]);
        index += 1;
      }
      out.push({ type: "code", language: fence[2] === "" ? null : fence[2], text: body.join("\n") });
      continue;
    }

    const heading = ATX_HEADING.exec(line);
    if (heading !== null) {
      out.push({
        type: "heading",
        level: heading[1].length,
        children: parseInline(heading[2] ?? ""),
      });
      index += 1;
      continue;
    }

    if (RULE.test(line)) {
      out.push({ type: "rule" });
      index += 1;
      continue;
    }

    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (index < lines.length) {
        if (QUOTE.test(lines[index])) {
          body.push(lines[index].replace(QUOTE, ""));
          index += 1;
          continue;
        }
        // A quote runs on through unmarked lines, the way a paragraph does,
        // and stops at a blank one.
        if (isBlank(lines[index]) || startsBlock(lines[index])) break;
        body.push(lines[index]);
        index += 1;
      }
      out.push({ type: "quote", children: parseBlocks(body) });
      continue;
    }

    if (
      line.includes("|") &&
      index + 1 < lines.length &&
      TABLE_DELIMITER.test(lines[index + 1]) &&
      splitRow(lines[index]).length === splitRow(lines[index + 1]).length
    ) {
      const head = inlineCells(lines[index]);
      const align = splitRow(lines[index + 1]).map(alignmentOf);
      index += 2;
      const rows: (readonly (readonly Inline[])[])[] = [];
      while (index < lines.length && lines[index].includes("|") && !isBlank(lines[index])) {
        rows.push(inlineCells(lines[index]));
        index += 1;
      }
      out.push({ type: "table", head, align, rows });
      continue;
    }

    const bullet = bulletOf(line);
    if (bullet !== null) {
      const items: (readonly Block[])[] = [];
      let tight = true;
      let pendingBlank = false;

      while (index < lines.length) {
        if (isBlank(lines[index])) {
          pendingBlank = true;
          index += 1;
          continue;
        }
        const next = bulletOf(lines[index]);
        // A deeper bullet was already swallowed by the item above it, so
        // anything at a different indent here closes this list. So does a
        // change of marker kind: `1.` after `-` is a second list, not a
        // seventh item.
        if (next === null || next.indent !== bullet.indent || next.ordered !== bullet.ordered) {
          break;
        }
        if (items.length > 0 && pendingBlank) tight = false;
        pendingBlank = false;

        const body: string[] = [next.content];
        index += 1;
        while (index < lines.length) {
          if (isBlank(lines[index])) {
            let lookahead = index;
            while (lookahead < lines.length && isBlank(lines[lookahead])) lookahead += 1;
            if (
              lookahead < lines.length &&
              indentOf(lines[lookahead]) >= next.contentIndent &&
              bulletOf(lines[lookahead])?.indent !== bullet.indent
            ) {
              body.push("");
              index = lookahead;
              tight = false;
              continue;
            }
            break;
          }
          if (indentOf(lines[index]) >= next.contentIndent) {
            body.push(lines[index].slice(next.contentIndent));
            index += 1;
            continue;
          }
          // A wrapped line under a bullet needs no indent to belong to it.
          if (!startsBlock(lines[index])) {
            body.push(lines[index].trim());
            index += 1;
            continue;
          }
          break;
        }
        items.push(parseBlocks(body));
      }

      out.push({ type: "list", ordered: bullet.ordered, start: bullet.start, tight, items });
      continue;
    }

    // Four spaces of indent with no list above it is a code block.
    if (/^ {4}/.test(line)) {
      const body: string[] = [];
      while (index < lines.length && (/^ {4}/.test(lines[index]) || isBlank(lines[index]))) {
        body.push(lines[index].slice(4));
        index += 1;
      }
      while (body.length > 0 && body[body.length - 1].trim() === "") body.pop();
      out.push({ type: "code", language: null, text: body.join("\n") });
      continue;
    }

    const paragraph: string[] = [];
    while (index < lines.length && !isBlank(lines[index]) && !startsBlock(lines[index])) {
      paragraph.push(lines[index]);
      index += 1;
    }
    // A `===` or `---` under a paragraph makes it a heading after the fact,
    // which is why the underline can only be recognised from here.
    const underline = index < lines.length ? SETEXT.exec(lines[index]) : null;
    if (underline !== null && paragraph.length > 0) {
      index += 1;
      out.push({
        type: "heading",
        level: underline[1][0] === "=" ? 1 : 2,
        children: parseInlineLines(paragraph),
      });
      continue;
    }
    if (paragraph.length > 0) {
      out.push({ type: "paragraph", children: parseInlineLines(paragraph) });
      continue;
    }

    // Nothing claimed the line; take it as text so the loop always advances.
    out.push({ type: "paragraph", children: parseInline(line) });
    index += 1;
  }

  return out;
}
