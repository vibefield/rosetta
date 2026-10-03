// Bound the text handed to the transcript's Markdown renderer. The component
// can lower this budget further to fit the available reading width; the full
// message is kept separately and is only rendered in the file viewer.
export const MESSAGE_PREVIEW_CHARACTERS = 2400;
export const MESSAGE_PREVIEW_LINES = 32;

export interface MessageExcerpt {
  readonly text: string;
  readonly omitted: boolean;
}

export function messageExcerpt(
  text: string,
  budget = MESSAGE_PREVIEW_CHARACTERS,
): MessageExcerpt {
  let end = Math.min(text.length, Math.max(0, budget), MESSAGE_PREVIEW_CHARACTERS);
  let lines = 1;
  for (let at = 0; at < end; at += 1) {
    if (text[at] === "\n" && ++lines > MESSAGE_PREVIEW_LINES) {
      end = at;
      break;
    }
  }
  if (end === text.length) return { text, omitted: false };

  // Prefer a paragraph or line boundary near the end of the allowance.
  // Otherwise keep the last whole word. Never split a UTF-16 surrogate pair.
  const prefix = text.slice(0, end);
  const paragraph = prefix.lastIndexOf("\n\n");
  const line = prefix.lastIndexOf("\n");
  const word = Math.max(prefix.lastIndexOf(" "), prefix.lastIndexOf("\t"));
  const boundary = [paragraph, line, word].find((at) => at >= end * 0.75);
  if (boundary !== undefined) end = boundary;
  if (end > 0 && /[\uD800-\uDBFF]/.test(text[end - 1]!)) end -= 1;
  let excerpt = text.slice(0, end).trimEnd();
  // Do not finish on a heading or an empty fence whose content is omitted.
  // Keep a heading-only introduction when it is all that fits.
  const dangling = /(?:^|\n) {0,3}(?:#{1,6}\s+[^\n]*|`{3,}[^\n]*|~{3,}[^\n]*)$/;
  for (let match = dangling.exec(excerpt); match !== null && match.index > 0; match = dangling.exec(excerpt)) {
    const before = excerpt.slice(0, match.index).trimEnd();
    if (before === "") break;
    excerpt = before;
  }
  return { text: excerpt, omitted: true };
}

/** Character counts stay meaningful for prose, source code and CJK text. */
export function messageCharacterCount(text: string): number {
  let count = 0;
  for (const _character of text) count += 1;
  return count;
}
