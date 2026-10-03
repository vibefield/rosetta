// The highlighter's whole public surface: a language, and one function that
// turns text into lines of tokens.

import { splitLines } from "./tokenize";
import type { Language } from "./languages";
import type { TokenLine } from "./types";

export { extensionOf, languageFor, PLAIN_TEXT } from "./languages";
export type { Language } from "./languages";
export type { Token, TokenKind, TokenLine } from "./types";

/** Tokenize `text` and cut it into lines. Pure and synchronous: a viewer that
 * has the bytes has the highlighting, with no second render to wait for. */
export function highlight(text: string, language: Language): readonly TokenLine[] {
  return splitLines(language.tokenize(text));
}
