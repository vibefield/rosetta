// No decorative meters: a meter fills only from a real fraction; an amount
// nobody knows sweeps (`data-indeterminate`) instead of resting at a fixed
// sliver that reads as progress. The server render covers the faces the room
// draws before the index has said anything; the source scan keeps a literal
// percentage width from coming back.
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { HistoryRoom } from "../../room/src/HistoryRoom.tsx";
import { Meter, Veil, meterWidth } from "../../room/src/components/ui/index.ts";
import { createDemoPorts } from "../../room/src/fixtures/index.ts";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const { createElement } = fromRoom("react");
const { renderToStaticMarkup } = fromRoom("react-dom/server");

test("the face drawn while reaching the index sweeps, with no invented width", () => {
  const markup = renderToStaticMarkup(createElement(HistoryRoom, { ...createDemoPorts(), storage: null }));
  assert.match(markup, /<div class="ah-meter" data-indeterminate="" aria-hidden="true"><i><\/i><\/div>/);
  assert.doesNotMatch(markup, /width:\s*(8|30)%/);
});

test("a meter fills from a real fraction and sweeps without one", () => {
  assert.equal(
    renderToStaticMarkup(createElement(Meter, { fraction: null })),
    '<div class="ah-meter" data-indeterminate="" aria-hidden="true"><i></i></div>',
  );
  assert.equal(
    renderToStaticMarkup(createElement(Meter, { fraction: 0.25 })),
    '<div class="ah-meter" aria-hidden="true"><i style="width:25.0%"></i></div>',
  );
  assert.equal(meterWidth(0), "2.0%", "a started step keeps a sliver");
  assert.equal(meterWidth(1.4), "100.0%");
});

test("a veil with no fraction sweeps under its label; with one it fills", () => {
  const idle = renderToStaticMarkup(createElement(Veil, { label: "Preparing activity summaries…", progress: null }));
  assert.match(idle, /role="status" aria-label="Preparing activity summaries…"/);
  assert.match(idle, /data-indeterminate=""/);
  assert.doesNotMatch(idle, /width:/);
  const measured = renderToStaticMarkup(createElement(Veil, { label: "preparing", progress: 0.5 }));
  assert.match(measured, /<i style="width:50.0%"><\/i>/);
  assert.doesNotMatch(measured, /data-indeterminate/);
});

function sources(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) return entry.name === "node_modules" || entry.name === "dist" ? [] : sources(file);
    return /\.tsx?$/.test(entry.name) ? [file] : [];
  });
}

// Every percentage written as a string literal, wherever it is assigned:
// a fixed sliver hides as easily in `cond ? "30%" : "8%"` or a variable as in
// `width: "8%"`. Gradient stops and a full-height frame are layout, not
// progress, and are the only literal percentages the components may hold.
const LITERAL_PERCENT = /["'`][0-9.]+%["'`]/g;
const LAYOUT_PERCENT = /(?:offset=|height:\s*)["'`][0-9.]+%["'`]/g;

function literalPercents(text) {
  return text.replace(LAYOUT_PERCENT, "").match(LITERAL_PERCENT) ?? [];
}

test("the scan finds the fixed slivers this check exists to keep out", () => {
  // The five shapes the decorative meters had before they were removed.
  for (const shape of [
    `<i style={{ width: "8%" }} />`,
    `style={{ width: connecting ? "30%" : "8%" }}`,
    `const width = progress === null ? "8%" : \`\${pct}%\`;`,
    `const meter = building ? "8%" : null;`,
  ]) {
    assert.notDeepEqual(literalPercents(shape), [], shape);
  }
  assert.deepEqual(literalPercents(`<stop offset="55%" /> style={{ height: "100%" }}`), []);
});

test("no room or app component holds a literal percentage outside layout", () => {
  const roots = [path.join(ROOM, "src"), fileURLToPath(new URL("../renderer/src/", import.meta.url))];
  const offenders = roots
    .flatMap(sources)
    .filter((file) => literalPercents(readFileSync(file, "utf8")).length > 0)
    .map((file) => path.relative(ROOM, file));
  assert.deepEqual(offenders, []);
});
