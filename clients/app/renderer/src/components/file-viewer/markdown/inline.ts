// Inline markdown: the marks inside one paragraph's worth of text.
//
// A left-to-right scanner rather than a chain of replacements. Replacement
// chains are how a naive renderer ends up emphasising the middle of a URL and
// bolding across a code span — the order of the passes decides the answer,
// and there is no order that is right for every document.

import type { Inline } from "./types";

/** `[label](href)` and `![alt](src)`, with the optional "title" ignored. */
const LINK = /^!?\[((?:[^[\]\\]|\\.|\[[^\]]*\])*)\]\(\s*(<[^>]*>|[^\s)]*)(?:\s+"[^"]*")?\s*\)/;
const AUTOLINK = /^<((?:https?|mailto|file):[^>\s]+)>/;
const BARE_URL = /^https?:\/\/[^\s<>()[\]"']+[^\s<>()[\]"'.,;:!?]/;
/** A run of backticks opens a code span that the same run closes. */
const CODE_FENCE = /^`+/;

interface Delimiter {
  readonly mark: string;
  readonly type: "strong" | "emphasis" | "strike";
}

const DELIMITERS: readonly Delimiter[] = [
  { mark: "***", type: "strong" },
  { mark: "___", type: "strong" },
  { mark: "**", type: "strong" },
  { mark: "__", type: "strong" },
  { mark: "~~", type: "strike" },
  { mark: "*", type: "emphasis" },
  { mark: "_", type: "emphasis" },
];

/** `_` inside `snake_case` is a word character, not an emphasis mark. The
 * test is what surrounds the run, which is why it cannot be a regex on the
 * mark alone. */
function opensRun(text: string, at: number, mark: string): boolean {
  const after = text[at + mark.length] ?? "";
  if (after === "" || /\s/.test(after)) return false;
  if (mark[0] !== "_") return true;
  const before = text[at - 1] ?? "";
  return !/[A-Za-z0-9]/.test(before);
}

function closesRun(text: string, at: number, mark: string): boolean {
  const before = text[at - 1] ?? "";
  if (before === "" || /\s/.test(before)) return false;
  if (mark[0] !== "_") return true;
  const after = text[at + mark.length] ?? "";
  return !/[A-Za-z0-9]/.test(after);
}

/** Where `mark` next closes a run opened at `from`, or -1. */
function findClose(text: string, from: number, mark: string): number {
  let at = from;
  while (at < text.length) {
    const next = text.indexOf(mark, at);
    if (next === -1) return -1;
    if (text[next - 1] === "\\") {
      at = next + mark.length;
      continue;
    }
    if (closesRun(text, next, mark)) return next;
    at = next + mark.length;
  }
  return -1;
}

/**
 * @param insideLink Suppresses links inside a link's own label. `[https://x.dev](https://y.dev)`
 *   is a real and common way to write a README line, and autolinking the label
 *   puts an `<a>` inside an `<a>` — markup no browser is obliged to keep.
 */
export function parseInline(text: string, insideLink = false): readonly Inline[] {
  const out: Inline[] = [];
  let plain = "";
  let index = 0;

  const flush = (): void => {
    if (plain === "") return;
    out.push({ type: "text", value: plain });
    plain = "";
  };
  const push = (node: Inline): void => {
    flush();
    out.push(node);
  };

  while (index < text.length) {
    const rest = text.slice(index);
    const char = text[index];

    if (char === "\\" && index + 1 < text.length && /[\\`*_{}[\]()#+\-.!<>~|]/.test(text[index + 1])) {
      plain += text[index + 1];
      index += 2;
      continue;
    }

    if (char === "`") {
      const fence = CODE_FENCE.exec(rest)?.[0] ?? "`";
      const close = text.indexOf(fence, index + fence.length);
      if (close !== -1) {
        const body = text.slice(index + fence.length, close);
        // CommonMark strips one space on each side, so `` ` `` can hold a
        // backtick without the spaces becoming content.
        const trimmed =
          body.length > 2 && body.startsWith(" ") && body.endsWith(" ") ? body.slice(1, -1) : body;
        push({ type: "code", value: trimmed });
        index = close + fence.length;
        continue;
      }
    }

    // An image inside a link label is legal markup — a linked thumbnail — so
    // only the link half of this is suppressed.
    if (char === "[" || (char === "!" && text[index + 1] === "[")) {
      const found = LINK.exec(rest);
      const isImage = found !== null && found[0].startsWith("!");
      if (found !== null && (isImage || !insideLink)) {
        const target = found[2].replace(/^<|>$/g, "");
        if (isImage) {
          push({ type: "image", src: target, alt: found[1] });
        } else {
          push({ type: "link", href: target, children: parseInline(found[1], true) });
        }
        index += found[0].length;
        continue;
      }
    }

    if (char === "<" && !insideLink) {
      const found = AUTOLINK.exec(rest);
      if (found !== null) {
        push({ type: "link", href: found[1], children: [{ type: "text", value: found[1] }] });
        index += found[0].length;
        continue;
      }
    }

    if (char === "h" && !insideLink && (index === 0 || /[\s(]/.test(text[index - 1] ?? " "))) {
      const found = BARE_URL.exec(rest);
      if (found !== null) {
        push({ type: "link", href: found[0], children: [{ type: "text", value: found[0] }] });
        index += found[0].length;
        continue;
      }
    }

    const delimiter = DELIMITERS.find(
      (candidate) => rest.startsWith(candidate.mark) && opensRun(text, index, candidate.mark),
    );
    if (delimiter !== undefined) {
      const close = findClose(text, index + delimiter.mark.length, delimiter.mark);
      if (close !== -1) {
        const inner = parseInline(text.slice(index + delimiter.mark.length, close), insideLink);
        // `***bold italic***` is one mark carrying two, and nesting it here
        // keeps the delimiter table flat.
        push(
          delimiter.mark.length === 3
            ? { type: "strong", children: [{ type: "emphasis", children: inner }] }
            : { type: delimiter.type, children: inner },
        );
        index = close + delimiter.mark.length;
        continue;
      }
    }

    plain += char;
    index += 1;
  }

  flush();
  return out;
}

/** Split a paragraph's lines into inline nodes, honouring the two ways
 * markdown spells a hard break. */
export function parseInlineLines(lines: readonly string[]): readonly Inline[] {
  const out: Inline[] = [];
  lines.forEach((line, index) => {
    const hard = /(?: {2,}|\\)$/.test(line);
    out.push(...parseInline(hard ? line.replace(/(?: {2,}|\\)$/, "") : line.trimEnd()));
    if (index === lines.length - 1) return;
    out.push(hard ? { type: "break" } : { type: "text", value: " " });
  });
  return out;
}
