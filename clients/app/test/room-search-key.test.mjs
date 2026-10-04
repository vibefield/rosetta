// RO-14 — the room prints only a key its host binds. The room binds no key of
// its own (the host's window does: rosetta's app binds ⌘K), so the launcher's
// legend and the empty transcript's sentence follow what the host says in
// `search.shortcut`, and a host with no search hears no key at all.
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { SearchLauncher } from "../../room/src/components/search/SearchLauncher.tsx";
import { emptyTranscriptPrompt, searchKeyLegend, shortcutLabel } from "../../room/src/components/search/shortcut.ts";

// The room's own React renders its components: one copy, the one they import.
const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const { createElement } = fromRoom("react");
const { renderToStaticMarkup } = fromRoom("react-dom/server");

test("the launcher's legend is the key the host says it binds, or none", () => {
  assert.equal(searchKeyLegend(null), null, "no search: no launcher, no key");
  assert.equal(searchKeyLegend({}), shortcutLabel("K"), "absent: the room's own label for ⌘K");
  assert.equal(searchKeyLegend({ shortcut: undefined }), shortcutLabel("K"));
  assert.equal(searchKeyLegend({ shortcut: null }), null, "null: the host binds no key");
  assert.equal(searchKeyLegend({ shortcut: { label: "⌥Space" } }), "⌥Space", "a label is printed as given");
});

test("the empty transcript names a key only when the launcher prints one", () => {
  assert.equal(emptyTranscriptPrompt(null), "Pick a session.");
  assert.equal(
    emptyTranscriptPrompt(shortcutLabel("K")),
    `Pick a session — or press ${shortcutLabel("K")} to search, which is the front door.`,
  );
  assert.equal(
    emptyTranscriptPrompt("⌥Space"),
    "Pick a session — or press ⌥Space to search, which is the front door.",
  );
});

test("the launcher draws its legend from the prop, and none for null", () => {
  const launcher = (shortcut) =>
    renderToStaticMarkup(
      createElement(SearchLauncher, { query: "", placeholder: "Search", shortcut, onOpen() {}, onClear() {} }),
    );
  assert.match(launcher("⌥Space"), /<kbd class="ah-kbd" aria-hidden="true">⌥Space<\/kbd>/);
  assert.doesNotMatch(launcher(null), /<kbd/, "a host that binds no key gets no legend");
  assert.match(launcher(null), /Search<\/span>/, "the launcher itself still draws");
});

test("no room source prints a key of its own: every legend is shortcutLabel's or the host's", () => {
  const SRC = path.join(ROOM, "src");
  const sources = (function walk(dir) {
    return readdirSync(dir).flatMap((name) => {
      const full = path.join(dir, name);
      return statSync(full).isDirectory() ? walk(full) : /\.(ts|tsx)$/.test(name) ? [full] : [];
    });
  })(SRC);
  const printed = [];
  for (const file of sources) {
    const code = readFileSync(file, "utf8").replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:])\/\/.*$/gm, "$1");
    if (/⌘\s?[A-Za-z]|\bCtrl\s[A-Za-z]/.test(code)) printed.push(path.relative(SRC, file));
  }
  assert.deepEqual(printed, []);
});
