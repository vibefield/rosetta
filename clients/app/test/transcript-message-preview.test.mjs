import assert from "node:assert/strict";
import { test } from "node:test";
import {
  MESSAGE_PREVIEW_CHARACTERS,
  MESSAGE_PREVIEW_LINES,
  messageCharacterCount,
  messageExcerpt,
} from "../renderer/src/components/transcript/message-preview.ts";
import {
  handleForMessage,
  messageFileSource,
} from "../renderer/src/components/transcript/message-file-source.ts";

test("short messages retain their exact Markdown, whitespace and line breaks", () => {
  const text = "# Review\r\n\r\nKeep **these words**, [this link](https://example.com), and 尾声.  \n";
  assert.deepEqual(messageExcerpt(text), { text, omitted: false });
  assert.deepEqual(messageExcerpt(""), { text: "", omitted: false });
});

test("large prose and dense line-based messages produce bounded prefixes", () => {
  const long = "A paragraph about the work. ".repeat(4000);
  const prose = messageExcerpt(long);
  assert.equal(prose.omitted, true);
  assert.ok(prose.text.length <= MESSAGE_PREVIEW_CHARACTERS);
  assert.ok(long.startsWith(prose.text));
  const lines = messageExcerpt(Array.from({ length: 500 }, (_, i) => `- item ${i}`).join("\n"));
  assert.equal(lines.omitted, true);
  assert.ok(lines.text.split("\n").length <= MESSAGE_PREVIEW_LINES);
});

test("smaller layout budgets preserve word boundaries and Unicode pairs", () => {
  const text = "Read the introduction.\n\nThen review the details carefully. ".repeat(100);
  const wide = messageExcerpt(text, 500);
  const narrow = messageExcerpt(text, 240);
  assert.ok(narrow.text.length < wide.text.length);
  assert.ok(text.startsWith(narrow.text));
  assert.ok(/\s/.test(text[narrow.text.length]));
  assert.deepEqual(messageExcerpt("a😀b", 2), { text: "a", omitted: true });
  assert.deepEqual(messageExcerpt(text, 0), { text: "", omitted: true });
  assert.equal(messageCharacterCount("a😀尾声"), 4);
});

test("excerpts do not end on a heading or empty code block with its body omitted", () => {
  const introduction = "Here are the completed changes.";
  const heading = `${introduction}\n\n## Next steps`;
  assert.equal(messageExcerpt(`${heading}\n\n${"Follow up. ".repeat(20)}`, heading.length).text, introduction);
  const fence = `${introduction}\n\n\`\`\`ts`;
  assert.equal(messageExcerpt(`${fence}\nconst ready = true;\n\`\`\``, fence.length).text, introduction);
});

test("the Markdown viewer gets the entire message, including text beyond the preview", async () => {
  const text = "# Full message\n\n" + "A long body. ".repeat(5000) + "\n\n**LAST LINE — 尾声 😀**\n";
  for (const role of ["user", "agent"]) {
    const handle = handleForMessage({ key: `s1:t1:${role}`, role, text });
    assert.equal(handle.name, role === "user" ? "User prompt.md" : "Agent reply.md");
    assert.equal(handle.path, null);
    const bytes = await messageFileSource.read(handle, new AbortController().signal);
    assert.equal(handle.size, bytes.byteLength);
    assert.equal(new TextDecoder().decode(bytes), text);
    assert.ok(!messageExcerpt(text).text.includes("LAST LINE"));
  }
});

test("message snapshots remain separate, abortable, and reject other file sources", async () => {
  const first = handleForMessage({ key: "s1:t1", role: "user", text: "first" });
  const second = handleForMessage({ key: "s2:t1", role: "agent", text: "second" });
  assert.notEqual(first.key, second.key);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(messageFileSource.read(first, controller.signal), { name: "AbortError" });
  await assert.rejects(messageFileSource.read({ ...first, ref: null }, new AbortController().signal));
  assert.equal(new TextDecoder().decode(await messageFileSource.read(second, new AbortController().signal)), "second");
});
