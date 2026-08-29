// What a highlighter is allowed to say about a piece of code.
//
// The kind set is CLOSED, and deliberately small. DESIGN.md §2.6 gives the
// room eight curated accents for closed sets — "these group and label; they
// never signal state" — and a token kind is exactly that: a label on a span
// of someone else's document. Six of the eight are spent here; red and pink
// are left alone, because red means failure everywhere else in this room and
// a reader must not learn a second meaning for it.
//
// This is a HIGHLIGHTER, not a parser. It knows a string from a comment from
// a keyword. It does not resolve types, scopes or imports, and nothing built
// on it may claim it does.

export type TokenKind =
  /** `// like this` — the quietest step of the ramp, never a hue. */
  | "comment"
  /** A literal run of text: quoted, templated, or heredoc'd. */
  | "string"
  /** A numeric literal. Shares its accent with `constant`: both are values. */
  | "number"
  /** `fn`, `const`, `impl`, `async` — the language's own words. */
  | "keyword"
  /** A named type. Also markup's attribute names, which play the same part. */
  | "type"
  /** A name in call position. */
  | "function"
  /** `true`, `null`, `None`, `self` — a value the language names for you. */
  | "constant"
  /** An object key, a config key, a markup attribute. */
  | "property"
  /** A markup element name. Shares its accent with `keyword`. */
  | "tag"
  /** Brackets, commas, arrows — structure the eye skips. */
  | "operator"
  /** Everything else: identifiers, prose, whitespace. */
  | "plain";

export interface Token {
  readonly kind: TokenKind;
  readonly text: string;
}

/** One source line's worth of tokens. Never contains a newline. */
export type TokenLine = readonly Token[];

/** How one flavour of string literal opens, closes and escapes. */
export interface StringRule {
  readonly open: string;
  /** Defaults to `open`. */
  readonly close?: string;
  /** May a newline appear inside it? */
  readonly multiline?: boolean;
  /** The escape character, if the literal has one. */
  readonly escape?: string;
}

/**
 * One language's shape, as data. The tokenizer is one function; a language is
 * a table — which is what makes adding one a five-line change rather than a
 * new code path.
 */
export interface Grammar {
  readonly lineComments?: readonly string[];
  readonly blockComments?: readonly (readonly [string, string])[];
  /** Tried in order, so `"""` must be listed before `"`. */
  readonly strings?: readonly StringRule[];
  readonly keywords?: ReadonlySet<string>;
  /** Values the language names: `true`, `nil`, `self`. */
  readonly constants?: ReadonlySet<string>;
  /** Types the language ships with: `u32`, `String`, `boolean`. */
  readonly types?: ReadonlySet<string>;
  /** Whether an identifier that starts with a capital reads as a type. */
  readonly capitalIsType?: boolean;
  /** `#[derive]`, `@override`, `#pragma` — a sigil-led annotation. */
  readonly annotation?: RegExp;
  /** An identifier before `:` is a key. True for JSON and object literals. */
  readonly propertyBeforeColon?: boolean;
  /**
   * `'a` is a lifetime, `'a'` is a character. Rust needs this told to it;
   * every other language with `'` strings does not.
   */
  readonly lifetimeQuote?: boolean;
  /** Sigil-led variables — shell's `$PATH`, Ruby's `@ivar`. */
  readonly variableSigils?: readonly string[];
  /**
   * `/` opens a literal as well as dividing. Only the JS family has the
   * ambiguity, and only what came BEFORE the slash can settle it — see
   * `startsRegex`.
   */
  readonly regexLiterals?: boolean;
}
