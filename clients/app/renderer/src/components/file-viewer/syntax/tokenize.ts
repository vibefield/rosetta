// One tokenizer, many languages. Everything a language differs by lives in
// its `Grammar` table; this file is the single pass that reads one.
//
// It is a lexer and nothing more: it never looks further than the next
// character, so it can be wrong about anything that needs a parser to know
// (a regex literal that looks like division, a generic that looks like a
// comparison). It is wrong quietly — the worst case is a span in the wrong
// colour, never a span that disappears.
//
// Every pattern here is sticky (`y`) and matched against the whole text at an
// offset. The obvious spelling — `pattern.exec(text.slice(index))` — copies
// the rest of the file once per token, which is quadratic and turns a 2 MB
// source file into a frozen window.

import type { Grammar, Token, TokenKind, TokenLine } from "./types";

/** `1_000`, `0xFF`, `1.5e-9`, `42u32` — with the suffix the systems
 * languages hang off the end. */
const NUMBER =
  /(?:0[xX][0-9a-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|(?:\d[\d_]*(?:\.\d[\d_]*)?|\.\d[\d_]*)(?:[eE][+-]?\d+)?)(?:[a-zA-Z_][a-zA-Z0-9_]*)?/y;
const IDENTIFIER = /[A-Za-z_$][A-Za-z0-9_$]*/y;
const WHITESPACE = /[ \t\r\n]+/y;
/** Structure the eye is meant to skip over. */
const OPERATOR_CHARS = new Set("+-*/%=<>!&|^~?:;,.()[]{}@#\\");

class Cursor {
  index = 0;
  /** The last token that was not pure whitespace, which is the only context a
   * one-character lookbehind ever needs. */
  lastKind: TokenKind | null = null;
  lastText = "";
  private readonly out: Token[] = [];
  /** Text emitted as `plain` but not yet flushed, so a run of plain
   * characters lands as one token instead of one token per character. */
  private pending = "";

  readonly text: string;

  // Spelled out rather than as a parameter property: the latter is TypeScript
  // that EMITS code, so a plain `node --strip-types` probe of this module
  // refuses the file. The tokenizer is worth being able to run that way.
  constructor(text: string) {
    this.text = text;
  }

  get done(): boolean {
    return this.index >= this.text.length;
  }

  peek(offset = 0): string {
    return this.text[this.index + offset] ?? "";
  }

  /** Does the source read `literal` right here? */
  looking(literal: string): boolean {
    return literal.length > 0 && this.text.startsWith(literal, this.index);
  }

  /** Match a STICKY pattern at the cursor. */
  match(pattern: RegExp): string | null {
    pattern.lastIndex = this.index;
    const found = pattern.exec(this.text);
    return found === null ? null : found[0];
  }

  /** The next character that is not a space or a tab, without moving. */
  peekNonSpace(from: number): string {
    let i = from;
    while (i < this.text.length && (this.text[i] === " " || this.text[i] === "\t")) i += 1;
    return this.text[i] ?? "";
  }

  /** Emit `text` as `kind` and advance past it. */
  take(kind: TokenKind, text: string): void {
    if (text.length === 0) return;
    const significant = text.trim();
    if (significant !== "") {
      this.lastKind = kind;
      this.lastText = significant;
    }
    if (kind === "plain") {
      this.pending += text;
    } else {
      this.flush();
      this.out.push({ kind, text });
    }
    this.index += text.length;
  }

  /** Retag the last token, for the lookahead a lexer can only do afterwards
   * (a JSON key is a string until the `:` proves otherwise). */
  retagLast(kind: TokenKind): void {
    const last = this.out[this.out.length - 1];
    if (last !== undefined && this.pending === "") {
      this.out[this.out.length - 1] = { kind, text: last.text };
    }
  }

  /** Consume through `closer`, or to the end of the text if it never closes —
   * an unterminated comment is a fact about the file, not a crash. */
  consumeUntil(opener: string, closer: string, escape?: string): string {
    const from = this.index + opener.length;
    if (escape === undefined) {
      const end = this.text.indexOf(closer, from);
      return end === -1 ? this.text.slice(this.index) : this.text.slice(this.index, end + closer.length);
    }
    let i = from;
    while (i < this.text.length) {
      if (this.text.startsWith(escape, i)) {
        i += escape.length + 1;
        continue;
      }
      if (this.text.startsWith(closer, i)) {
        return this.text.slice(this.index, i + closer.length);
      }
      i += 1;
    }
    return this.text.slice(this.index);
  }

  finish(): readonly Token[] {
    this.flush();
    return this.out;
  }

  private flush(): void {
    if (this.pending === "") return;
    this.out.push({ kind: "plain", text: this.pending });
    this.pending = "";
  }
}

function classifyIdentifier(word: string, grammar: Grammar, next: string): TokenKind {
  if (grammar.keywords?.has(word)) return "keyword";
  if (grammar.constants?.has(word)) return "constant";
  if (grammar.types?.has(word)) return "type";
  // A capital outranks the call position: `new Uint8Array(...)`,
  // `String::from(...)` and `<Button …>` are types being used, and painting
  // them as functions teaches the reader that the two are the same thing.
  // `HTML` and `X` are shouted constants or one letter, and a rule without
  // the lowercase test paints most of a C header amber.
  if (grammar.capitalIsType === true && /^[A-Z]/.test(word) && /[a-z]/.test(word)) return "type";
  if (next === "(") return "function";
  if (grammar.propertyBeforeColon === true && next === ":") return "property";
  return "plain";
}

/** `'a` is a lifetime and `'a'` is a character, and only the character after
 * the identifier tells them apart. */
function readsAsLifetime(cursor: Cursor): boolean {
  if (!/[A-Za-z_]/.test(cursor.peek(1))) return false;
  let offset = 1;
  while (/[A-Za-z0-9_]/.test(cursor.peek(offset))) offset += 1;
  return cursor.peek(offset) !== "'";
}

const LIFETIME = /'[A-Za-z0-9_]+/y;
/** A literal, with character classes read as units so a `/` inside `[^/]`
 * does not end it, and the flags taken along. */
const REGEX_LITERAL = /\/(?![*/])(?:\[(?:[^\]\\\n]|\\.)*\]|[^/\\\n[]|\\.)+\/[dgimsuvy]*/y;
/** Keywords a regex may follow. After anything else that is a VALUE — an
 * identifier, a number, a closing bracket — the slash is division. */
const REGEX_KEYWORDS: ReadonlySet<string> = new Set([
  "return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "case",
  "do", "else", "yield", "await", "throw",
]);

/**
 * Is the `/` at the cursor opening a literal, or dividing?
 *
 * No lexer can always be right about this — `a++ /b/ g` is legal division —
 * but the previous significant token answers it for every line anyone
 * actually writes, and being wrong costs a span its colour and nothing else.
 */
function startsRegex(cursor: Cursor): boolean {
  if (cursor.lastKind === null) return true;
  if (cursor.lastKind === "operator") return cursor.lastText !== ")" && cursor.lastText !== "]";
  if (cursor.lastKind === "keyword") return REGEX_KEYWORDS.has(cursor.lastText);
  return false;
}
/** Sigil-led variable patterns, built once per grammar rather than once per
 * character — the same quadratic trap the sticky patterns avoid. */
const sigilPatterns = new WeakMap<Grammar, readonly (readonly [string, RegExp])[]>();

function sigilsOf(grammar: Grammar): readonly (readonly [string, RegExp])[] {
  const cached = sigilPatterns.get(grammar);
  if (cached !== undefined) return cached;
  const built = (grammar.variableSigils ?? []).map(
    (sigil) =>
      [sigil, new RegExp(`${sigil.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}[A-Za-z_{][A-Za-z0-9_}]*`, "y")] as const,
  );
  sigilPatterns.set(grammar, built);
  return built;
}

export function tokenize(text: string, grammar: Grammar): readonly Token[] {
  const cursor = new Cursor(text);
  const sigils = sigilsOf(grammar);

  while (!cursor.done) {
    const char = cursor.peek();

    // Whitespace, in one run. Newlines included — lines are cut afterwards,
    // so a block comment stays one token across all of them.
    if (char === " " || char === "\t" || char === "\n" || char === "\r") {
      cursor.take("plain", cursor.match(WHITESPACE) ?? char);
      continue;
    }

    const block = grammar.blockComments?.find(([open]) => cursor.looking(open));
    if (block !== undefined) {
      cursor.take("comment", cursor.consumeUntil(block[0], block[1]));
      continue;
    }

    const lineComment = grammar.lineComments?.find((open) => cursor.looking(open));
    if (lineComment !== undefined) {
      const end = text.indexOf("\n", cursor.index);
      cursor.take("comment", text.slice(cursor.index, end === -1 ? text.length : end));
      continue;
    }

    if (grammar.annotation !== undefined) {
      const annotation = cursor.match(grammar.annotation);
      if (annotation !== null && annotation.length > 0) {
        cursor.take("type", annotation);
        continue;
      }
    }

    if (char === "'" && grammar.lifetimeQuote === true && readsAsLifetime(cursor)) {
      cursor.take("type", cursor.match(LIFETIME) ?? "'");
      continue;
    }

    if (char === "/" && grammar.regexLiterals === true && startsRegex(cursor)) {
      const literal = cursor.match(REGEX_LITERAL);
      if (literal !== null) {
        // A literal is text quoted from another language, which is what a
        // string is; it takes the same accent for the same reason.
        cursor.take("string", literal);
        continue;
      }
    }

    const string = grammar.strings?.find((rule) => cursor.looking(rule.open));
    if (string !== undefined) {
      const raw = cursor.consumeUntil(string.open, string.close ?? string.open, string.escape);
      // A single-line literal that ran past a newline never closed: stop at
      // the line, so one stray quote does not paint the rest of the file.
      const newline = string.multiline === true ? -1 : raw.indexOf("\n");
      cursor.take("string", newline === -1 ? raw : raw.slice(0, newline));
      if (grammar.propertyBeforeColon === true && cursor.peekNonSpace(cursor.index) === ":") {
        cursor.retagLast("property");
      }
      continue;
    }

    const sigil = sigils.find(([lead]) => cursor.looking(lead));
    if (sigil !== undefined) {
      const variable = cursor.match(sigil[1]);
      if (variable !== null) {
        cursor.take("constant", variable);
        continue;
      }
    }

    if (/[0-9]/.test(char) || (char === "." && /[0-9]/.test(cursor.peek(1)))) {
      const number = cursor.match(NUMBER);
      if (number !== null) {
        cursor.take("number", number);
        continue;
      }
    }

    const word = cursor.match(IDENTIFIER);
    if (word !== null) {
      const next = cursor.peekNonSpace(cursor.index + word.length);
      cursor.take(classifyIdentifier(word, grammar, next), word);
      continue;
    }

    cursor.take(OPERATOR_CHARS.has(char) ? "operator" : "plain", char);
  }

  return cursor.finish();
}

/**
 * Cut a token stream into lines. Tokens that span newlines — block comments,
 * template literals, docstrings — are split, and each piece keeps its kind,
 * which is the whole reason tokenizing runs over the file rather than over
 * each line in turn.
 */
export function splitLines(tokens: readonly Token[]): readonly TokenLine[] {
  const lines: Token[][] = [[]];
  for (const token of tokens) {
    if (!token.text.includes("\n")) {
      if (token.text.length > 0) lines[lines.length - 1].push(token);
      continue;
    }
    const pieces = token.text.split("\n");
    for (let index = 0; index < pieces.length; index += 1) {
      if (index > 0) lines.push([]);
      const piece = pieces[index];
      const trimmed = piece.endsWith("\r") ? piece.slice(0, -1) : piece;
      if (trimmed.length > 0) lines[lines.length - 1].push({ kind: token.kind, text: trimmed });
    }
  }
  return lines;
}
