// CSS, SCSS and Less. A stylesheet has three worlds rather than one — the
// selector, the property, the value — and the same identifier means a
// different thing in each. So the lexer carries the one bit of state that
// tells them apart: whether it is inside a block, and whether it is past the
// colon.

import type { Token, TokenKind } from "./types";

const IDENTIFIER = /-{0,2}[A-Za-z_][A-Za-z0-9_-]*/y;
const AT_RULE = /@[A-Za-z-]+/y;
const HEX = /#[0-9a-fA-F]{3,8}\b/y;
const NUMBER = /[+-]?(?:\d+\.?\d*|\.\d+)(?:%|[a-zA-Z]+)?/y;
const VARIABLE = /[$@][A-Za-z_][A-Za-z0-9_-]*/y;
const PSEUDO = /::?[A-Za-z-][A-Za-z0-9_-]*/y;
const ID_SELECTOR = /#[A-Za-z_-][A-Za-z0-9_-]*/y;
const CLASS_SELECTOR = /\.[A-Za-z_-][A-Za-z0-9_-]*/y;
const WHITESPACE = /[ \t\r\n]+/y;

/** Values the language names, as opposed to values an author invents. */
const VALUE_CONSTANTS = new Set([
  "auto", "inherit", "initial", "unset", "none", "revert", "currentColor", "transparent",
  "true", "false", "important",
]);

export function tokenizeCss(text: string): readonly Token[] {
  const out: Token[] = [];
  let index = 0;
  let depth = 0;
  /** Past the `:` of a declaration, where identifiers are values. */
  let inValue = false;

  const push = (kind: TokenKind, value: string): void => {
    if (value.length === 0) return;
    out.push({ kind, text: value });
    index += value.length;
  };

  const match = (pattern: RegExp): string | null => {
    pattern.lastIndex = index;
    const found = pattern.exec(text);
    return found === null ? null : found[0];
  };

  const through = (from: number, closer: string): string => {
    const at = text.indexOf(closer, from);
    return at === -1 ? text.slice(index) : text.slice(index, at + closer.length);
  };

  const peekNonSpace = (from: number): string => {
    let i = from;
    while (i < text.length && /[ \t\r\n]/.test(text[i]!)) i += 1;
    return text[i] ?? "";
  };

  while (index < text.length) {
    const char = text[index]!;

    if (/[ \t\r\n]/.test(char)) {
      push("plain", match(WHITESPACE) ?? char!);
      continue;
    }
    if (text.startsWith("/*", index)) {
      push("comment", through(index + 2, "*/"));
      continue;
    }
    // SCSS and Less only; plain CSS has no line comment, and a file that
    // uses one is not plain CSS.
    if (text.startsWith("//", index)) {
      const end = text.indexOf("\n", index);
      push("comment", text.slice(index, end === -1 ? text.length : end));
      continue;
    }
    if (char === '"' || char === "'") {
      push("string", through(index + 1, char));
      continue;
    }
    if (char === "{") {
      push("operator", "{");
      depth += 1;
      inValue = false;
      continue;
    }
    if (char === "}") {
      push("operator", "}");
      depth = Math.max(0, depth - 1);
      inValue = false;
      continue;
    }
    if (char === ";") {
      push("operator", ";");
      inValue = false;
      continue;
    }
    if (char === ":") {
      // Inside a block a colon opens a value; in a selector it opens a
      // pseudo-class, which is part of the name rather than a separator.
      if (depth > 0 && !inValue) {
        push("operator", ":");
        inValue = true;
        continue;
      }
      const pseudo = match(PSEUDO);
      push(pseudo === null ? "operator" : "constant", pseudo ?? ":");
      continue;
    }
    // A Less variable is a VALUE, so it is the value position that tells it
    // apart from an at-rule — not the nesting depth, which reads SCSS's
    // nested `@media` as a variable.
    if (char === "$" || (char === "@" && inValue)) {
      const variable = match(VARIABLE);
      if (variable !== null) {
        push("constant", variable);
        continue;
      }
    }
    if (char === "@") {
      const atRule = match(AT_RULE);
      if (atRule !== null) {
        push("keyword", atRule);
        continue;
      }
    }
    if (char === "!") {
      const bang = match(/![ \t]*[a-z]+/y);
      if (bang !== null) {
        push("keyword", bang);
        continue;
      }
    }
    if (char === "#") {
      const hex = match(HEX);
      if (hex !== null) {
        push("number", hex);
        continue;
      }
      // Not a colour, so an id selector.
      const id = match(ID_SELECTOR);
      push(id === null ? "operator" : "type", id ?? "#");
      continue;
    }
    if (char === ".") {
      const className = match(CLASS_SELECTOR);
      if (className !== null) {
        push("type", className);
        continue;
      }
    }
    // `-4px` is one negative length, not an operator and a length. `--x` and
    // `-webkit-` are not numbers, and the digit test is what separates them.
    if (
      /[0-9]/.test(char) ||
      ((char === "." || char === "-") && /[0-9.]/.test(text[index + 1] ?? ""))
    ) {
      const number = match(NUMBER);
      if (number !== null) {
        push("number", number);
        continue;
      }
    }

    const word = match(IDENTIFIER);
    if (word !== null) {
      const next = peekNonSpace(index + word.length);
      let kind: TokenKind;
      if (next === "(") kind = "function";
      else if (word.startsWith("--")) kind = "property";
      else if (inValue) kind = VALUE_CONSTANTS.has(word) ? "constant" : "plain";
      else if (depth > 0) kind = "property";
      else kind = "tag";
      push(kind, word);
      continue;
    }

    push("operator", char);
  }

  return out;
}
