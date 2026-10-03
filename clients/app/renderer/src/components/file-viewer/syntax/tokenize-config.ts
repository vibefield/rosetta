// YAML, TOML and INI. These are line-shaped languages: what a token means
// depends on where it sits in its line and almost never on the line before,
// so the lexer is a per-line reader rather than a character machine.
//
// The one exception is YAML's block scalar (`key: |`), whose body is prose
// that must not be lexed as config. That is carried across lines by
// indentation, which is the only state this file keeps.

import type { Token, TokenKind } from "./types";

const KEY = /(?:"[^"\n]*"|'[^'\n]*'|\[\[?[^\]\n]*\]\]?|[A-Za-z0-9_.$/-]+)(?=[ \t]*[:=])/y;
const SECTION = /\[\[?[^\]\n]*\]\]?/y;
const STRING = /"""[\s\S]*?"""|'''[\s\S]*?'''|"(?:[^"\\\n]|\\.)*"|'[^'\n]*'/y;
const NUMBER = /[+-]?(?:0[xXbBoO][0-9a-fA-F_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)/y;
const DATE = /\d{4}-\d{2}-\d{2}(?:[Tt ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:\d{2})?)?/y;
const ANCHOR = /[&*][A-Za-z0-9_-]+/y;
const WORD = /[A-Za-z_][A-Za-z0-9_.-]*/y;

const CONSTANTS = new Set([
  "true", "false", "null", "yes", "no", "on", "off", "True", "False", "Null", "None",
  "TRUE", "FALSE", "NULL", "~",
]);

export function tokenizeConfig(text: string): readonly Token[] {
  const out: Token[] = [];
  const push = (kind: TokenKind, value: string): void => {
    if (value.length > 0) out.push({ kind, text: value });
  };

  /** Indentation of the block scalar currently being consumed as prose, or
   * null when there is none. */
  let literalIndent: number | null = null;

  const lines = text.split("\n");
  lines.forEach((line, lineIndex) => {
    if (lineIndex > 0) push("plain", "\n");

    const indent = line.length - line.trimStart().length;
    if (literalIndent !== null) {
      if (line.trim() === "" || indent > literalIndent) {
        push("string", line);
        return;
      }
      literalIndent = null;
    }

    let index = 0;
    const match = (pattern: RegExp): string | null => {
      pattern.lastIndex = index;
      const found = pattern.exec(line);
      return found === null ? null : found[0];
    };
    const emit = (kind: TokenKind, value: string): void => {
      push(kind, value);
      index += value.length;
    };
    const skipSpace = (): void => {
      const space = match(/[ \t]+/y);
      if (space !== null) emit("plain", space);
    };

    skipSpace();
    if (index >= line.length) {
      push("plain", line.slice(index));
      return;
    }

    if (line[index] === "#" || line[index] === ";") {
      emit("comment", line.slice(index));
      return;
    }

    // A list item is a marker plus a value — or plus a key, for a list of
    // tables, which is why this falls through rather than returning.
    while (line[index] === "-" && (line[index + 1] === " " || index + 1 === line.length)) {
      emit("operator", "-");
      skipSpace();
    }

    const section = line[index] === "[" ? match(SECTION) : null;
    if (section !== null) {
      emit("type", section);
      skipSpace();
      if (line[index] === "#" || line[index] === ";") emit("comment", line.slice(index));
      return;
    }

    const key = match(KEY);
    if (key !== null) {
      emit("property", key);
      skipSpace();
      emit("operator", line[index] ?? "");
      skipSpace();
    }

    // Whatever is left of the line is a value.
    while (index < line.length) {
      const char = line[index]!;
      if (char === "#" && (index === 0 || /[ \t]/.test(line[index - 1] ?? " "))) {
        emit("comment", line.slice(index));
        break;
      }
      if (/[ \t]/.test(char)) {
        skipSpace();
        continue;
      }
      if ((char === "|" || char === ">") && index === line.length - 1) {
        emit("operator", char);
        literalIndent = indent;
        break;
      }
      if (char === '"' || char === "'") {
        const string = match(STRING);
        emit("string", string ?? line.slice(index));
        continue;
      }
      if (char === "&" || char === "*") {
        const anchor = match(ANCHOR);
        if (anchor !== null) {
          emit("type", anchor);
          continue;
        }
      }
      const date = match(DATE);
      if (date !== null) {
        emit("number", date);
        continue;
      }
      const number = match(NUMBER);
      if (number !== null) {
        emit("number", number);
        continue;
      }
      const word = match(WORD);
      if (word !== null) {
        emit(CONSTANTS.has(word) ? "constant" : "plain", word);
        continue;
      }
      emit(CONSTANTS.has(char) ? "constant" : "operator", char);
    }
  });

  return out;
}
