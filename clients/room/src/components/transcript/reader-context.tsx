// What the reader (PK3, `TranscriptReader`) tells the document it holds:
// that it is read-only, and the words its search matched, marked where the
// reply and the prompt say them. Only the reader provides either; the room
// marks a landing by its line and never by its words, so with no provider
// nothing here changes anything.
import { createContext, Fragment, useContext } from "react";
import type { ReactNode } from "react";

const ReaderModeContext = createContext(false);
export const ReaderModeProvider = ReaderModeContext.Provider;

/** True inside the reader: parts that open something draw as plain text. */
export function useReaderMode(): boolean {
  return useContext(ReaderModeContext);
}

const MatchTermsContext = createContext<readonly string[]>([]);
export const MatchTermsProvider = MatchTermsContext.Provider;

/** The words the reader marks, lower-cased; none outside a reader. */
export function useMatchTerms(): readonly string[] {
  return useContext(MatchTermsContext);
}

/** A query's words as the index matches them: each word on its own, a phrase
 * in quotes as one. */
export function matchTermsOf(query: string | null | undefined): readonly string[] {
  if (query === null || query === undefined) return [];
  const terms: string[] = [];
  for (const match of query.matchAll(/"([^"]*)"|(\S+)/g)) {
    const term = (match[1] ?? match[2] ?? "").replace(/"/g, "").trim().toLowerCase();
    if (term !== "" && !terms.includes(term)) terms.push(term);
  }
  // The longest first, so a phrase wins over a word inside it.
  return terms.sort((left, right) => right.length - left.length);
}

function escape(term: string): string {
  return term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function markText(value: string, pattern: RegExp): ReactNode {
  const parts = value.split(pattern);
  if (parts.length === 1) return value;
  // split() with one capture group puts every match at an odd index.
  return parts.map((part, at) =>
    at % 2 === 1 ? <mark key={at} className="tr-match">{part}</mark> : <Fragment key={at}>{part}</Fragment>,
  );
}

/** Marks every term in a run of text. Strings are marked; anything already
 * drawn (an image link) is kept as it is. */
export function markTerms(content: ReactNode, terms: readonly string[]): ReactNode {
  if (terms.length === 0) return content;
  const pattern = new RegExp(`(${terms.map(escape).join("|")})`, "gi");
  if (typeof content === "string") return markText(content, pattern);
  if (!Array.isArray(content)) return content;
  return content.map((part, at) =>
    typeof part === "string" ? <Fragment key={`m${at}`}>{markText(part, pattern)}</Fragment> : part,
  );
}
