// HTML, XML and SVG. Markup is not the C family wearing different keywords —
// it has two alternating worlds, text and tag, and a table-driven lexer that
// pretends otherwise paints attribute values as identifiers.
//
// A tag name takes the `tag` kind and an attribute name takes `property`,
// which share their accents with `keyword` and an object key. They are the
// same two roles in another syntax, and the room should not teach a reader
// two palettes for one idea.

import type { Token } from "./types";

const TAG_NAME = /[A-Za-z_][A-Za-z0-9_:.-]*/y;
/** Plain `href`, and the framework spellings a real page is full of:
 * `v-if`, `:class`, `@click`, `#ref`, `[prop]`, `(click)`. */
const ATTRIBUTE_NAME = /[A-Za-z_@:#$[(][-A-Za-z0-9_@:.$\])]*/y;
const ENTITY = /&[a-zA-Z][a-zA-Z0-9]*;|&#[0-9]+;|&#x[0-9a-fA-F]+;/y;

export function tokenizeMarkup(text: string): readonly Token[] {
  const out: Token[] = [];
  let index = 0;

  const push = (kind: Token["kind"], value: string): void => {
    if (value.length === 0) return;
    out.push({ kind, text: value });
    index += value.length;
  };

  /** Everything up to `end`, or the rest of the file when it never arrives. */
  const through = (from: number, closer: string): string => {
    const at = text.indexOf(closer, from);
    return at === -1 ? text.slice(index) : text.slice(index, at + closer.length);
  };

  const match = (pattern: RegExp): string | null => {
    pattern.lastIndex = index;
    const found = pattern.exec(text);
    return found === null ? null : found[0];
  };

  while (index < text.length) {
    if (text.startsWith("<!--", index)) {
      push("comment", through(index + 4, "-->"));
      continue;
    }
    if (text.startsWith("<![CDATA[", index)) {
      push("string", through(index + 9, "]]>"));
      continue;
    }
    if (text.startsWith("<!", index) || text.startsWith("<?", index)) {
      push("keyword", through(index + 2, ">"));
      continue;
    }

    if (text[index] === "<") {
      push("operator", text.startsWith("</", index) ? "</" : "<");
      const name = match(TAG_NAME);
      if (name !== null) push("tag", name);

      // Inside the tag until `>`: attribute, `=`, value, repeat.
      while (index < text.length && text[index] !== ">") {
        const char = text[index];
        if (char === " " || char === "\t" || char === "\n" || char === "\r") {
          push("plain", match(/[ \t\r\n]+/y) ?? char);
          continue;
        }
        if (char === "/") {
          push("operator", "/");
          continue;
        }
        if (char === "=") {
          push("operator", "=");
          continue;
        }
        if (char === '"' || char === "'") {
          push("string", through(index + 1, char));
          continue;
        }
        const attribute = match(ATTRIBUTE_NAME);
        if (attribute !== null) {
          push("property", attribute);
          continue;
        }
        // An unquoted value, or something this lexer has no name for.
        push("plain", char);
      }
      if (index < text.length) push("operator", ">");
      continue;
    }

    const entity = match(ENTITY);
    if (entity !== null) {
      push("constant", entity);
      continue;
    }

    // Text content, in one run up to the next markup character.
    const next = text.indexOf("<", index + 1);
    const ampersand = text.indexOf("&", index + 1);
    const stop = [next, ampersand].filter((at) => at !== -1).sort((a, b) => a - b)[0] ?? text.length;
    push("plain", text.slice(index, Math.max(stop, index + 1)));
  }

  return out;
}
