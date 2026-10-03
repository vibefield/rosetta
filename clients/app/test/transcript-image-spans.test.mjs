import assert from "node:assert/strict";
import { test } from "node:test";
import { parseMarkdown } from "../../room/src/components/file-viewer/markdown/index.ts";

function textNodes(tree, inLink = false) {
  return tree.flatMap(node => {
    if (node.type === "text") return inLink ? [] : [node];
    if (node.type === "link" || node.type === "code" || node.type === "image") return [];
    return [
      ...(node.children ? textNodes(node.children) : []),
      ...(node.items ? node.items.flatMap(item => textNodes(item)) : []),
      ...(node.head ? node.head.flatMap(cell => textNodes(cell)) : []),
      ...(node.rows ? node.rows.flatMap(row => row.flatMap(cell => textNodes(cell))) : []),
    ];
  });
}
function withoutSource(tree) { return JSON.parse(JSON.stringify(tree, (key, value) => key === "source" ? undefined : value)); }

test("Markdown annotations keep exact positions through Unicode, CRLF, tabs, nesting and tables", () => {
  const cases = [
    "😀 first [Image #1] then [Image #1].",
    "# **[Image #1]**\r\n\r\n> - before\r\n>   [Image #1] after",
    "- nested\n\t- [Image #1]\n\n  continued [Image #1]",
    "| Cell | Target |\n| --- | --- |\n| escaped \\| pipe | [Image #1] |",
    "plain\t[Image #1] and \\* escaped [Image #1]",
  ];
  for (const text of cases) {
    const tree = parseMarkdown(text, true);
    assert.deepEqual(withoutSource(tree), parseMarkdown(text));
    const plain = textNodes(tree);
    for (const match of text.matchAll(/\[Image #1\]/g)) {
      assert.ok(plain.some(node => {
        const index = node.source?.indexOf(match.index) ?? -1;
        return index >= 0 && node.value.slice(index, index + match[0].length) === match[0]
          && node.source.slice(index, index + match[0].length).every((at, i) => at === match.index + i);
      }), `missing exact source span at ${match.index} in ${JSON.stringify(text)}`);
    }
  }
});

test("code, fenced examples and link labels never receive attachment annotation targets", () => {
  const text = "`[Image #1]`\n\n```md\n[Image #1]\n```\n\n[Image #1](https://example.com)\n\n**[Image #1]** plain";
  const plain = textNodes(parseMarkdown(text, true));
  const positions = [...text.matchAll(/\[Image #1\]/g)].map(match => match.index);
  for (const position of positions.slice(0, -1)) assert.ok(!plain.some(node => node.source?.includes(position)));
  assert.ok(plain.some(node => node.source?.includes(positions.at(-1))));
});
