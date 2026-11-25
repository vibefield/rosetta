// Real layout of a diff whose lines are longer than its pane, in the commit
// viewer: a long line wraps inside its own row, the row is as tall as its
// text, and no row's text reaches a neighbour, the diffstat or the pane's
// edge. A hidden window and a disposable profile; nothing is typed or clicked
// through the OS.
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-diff-wrap-"));
app.setPath("userData", path.join(temporary, "profile"));
app.on("window-all-closed", () => {});
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error("Diff wrap check timed out"); app.exit(1); }, 60_000);
let win, server;
let failed = false;
const errors = [], screenshots = [];
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message, ...args) {
  for (let i = 0; i < 160; i++) { if (await check(fn, ...args)) return; await pause(25); }
  throw new Error(message);
}
async function shot(name) {
  const file = path.join(temporary, `${name}.png`);
  await writeFile(file, (await win.webContents.capturePage()).toPNG());
  screenshots.push(file);
}

// What the drawn rows measure, top to bottom. Everything a reader could see
// go wrong is a number here: where each row starts and ends, where its text
// ends, and where its gutter mark sits.
function measure() {
  const scroller = document.querySelector(".ah-diff");
  if (scroller === null) return null;
  const stat = scroller.querySelector(".ah-diff__stat").getBoundingClientRect();
  const sizer = scroller.querySelector(".ah-code__sizer");
  const rows = [...sizer.querySelectorAll(".ah-code__line")]
    .map(row => {
      const box = row.getBoundingClientRect();
      const code = row.querySelector("code");
      const mark = row.querySelector(".ah-diff__mark");
      let markTop = null;
      if (mark !== null && mark.textContent.trim() !== "") {
        const range = document.createRange();
        range.selectNodeContents(mark);
        markTop = range.getBoundingClientRect().top - box.top;
      }
      let textBottom = box.bottom, textRight = box.right;
      if (code !== null && code.textContent !== "") {
        const range = document.createRange();
        range.selectNodeContents(code);
        const text = range.getBoundingClientRect();
        textBottom = text.bottom; textRight = text.right;
      }
      return { index: Number(row.dataset.index), kind: row.dataset.diff, top: box.top, bottom: box.bottom,
        height: box.height, right: box.right, textTop: code === null ? box.top : code.getBoundingClientRect().top,
        textBottom, textRight, markTop, length: code === null ? 0 : code.textContent.length };
    })
    .sort((a, b) => a.index - b.index);
  return { rows, statBottom: stat.bottom, scrollTop: scroller.scrollTop, clientWidth: scroller.clientWidth,
    scrollWidth: scroller.scrollWidth, sizerHeight: sizer.getBoundingClientRect().height,
    sizerTop: sizer.getBoundingClientRect().top, sizerBottom: sizer.getBoundingClientRect().bottom };
}

/** The rows drawn are whole, in order, and touch without crossing. */
function assertSeparate(seen, where) {
  assert.ok(seen.rows.length > 0, `${where}: no rows drawn`);
  assert.ok(seen.scrollWidth <= seen.clientWidth, `${where}: the diff scrolls sideways (${seen.scrollWidth} > ${seen.clientWidth})`);
  for (const [at, row] of seen.rows.entries()) {
    assert.ok(row.height >= 19, `${where}: row ${row.index} is shorter than one line (${row.height})`);
    assert.ok(row.textBottom <= row.bottom + 0.5, `${where}: row ${row.index}'s text ends ${row.textBottom - row.bottom}px below its row`);
    assert.ok(row.textTop >= row.top - 0.5, `${where}: row ${row.index}'s text starts above its row`);
    assert.ok(row.textRight <= row.right + 0.5, `${where}: row ${row.index}'s text runs past its row's edge`);
    if (row.markTop !== null) assert.ok(Math.abs(row.markTop) <= 2, `${where}: row ${row.index}'s mark is ${row.markTop}px down its row, not on its first line`);
    const next = seen.rows[at + 1];
    if (next === undefined || next.index !== row.index + 1) continue;
    assert.ok(Math.abs(next.top - row.bottom) <= 0.5, `${where}: rows ${row.index} and ${next.index} are ${next.top - row.bottom}px apart (negative is an overlap)`);
  }
}

async function settled(message) {
  // Two equal readings a beat apart: the virtualizer has placed what it measured.
  let last = "";
  for (let i = 0; i < 80; i++) {
    const seen = await check(measure);
    const now = JSON.stringify(seen?.rows.map(row => [row.index, Math.round(row.top), Math.round(row.height)]) ?? null);
    if (seen !== null && seen.rows.length > 0 && now === last) return seen;
    last = now;
    await pause(50);
  }
  throw new Error(message);
}

async function run() {
  try {
    await app.whenReady();
    clearInterval(pulse);
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 1180, height: 760, show: false,
      webPreferences: { contextIsolation: true, nodeIntegration: false, backgroundThrottling: false } });
    win.webContents.on("console-message", event => { if (event.level === "error") errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/diff-wrap.html`);
    await until(() => document.querySelectorAll(".ah-diff .ah-code__line").length === 9, "The table's nine rows were not drawn");
    await pause(320); // the dialog's entrance

    // The table: every row is drawn, so the sizer is exactly their sum.
    const wide = await settled("The table's rows never settled");
    assertSeparate(wide, "wide");
    assert.deepEqual(wide.rows.map(row => row.kind), ["hunk", "context", "context", "context", "del", "add", "add", "context", "context"]);
    assert.ok(wide.rows[0].top >= wide.statBottom - 0.5, "wide: the first row sits under the diffstat");
    assert.ok(Math.abs(wide.rows[0].top - wide.sizerTop) <= 0.5, "wide: the first row does not start the sizer");
    assert.ok(Math.abs(wide.rows.at(-1).bottom - wide.sizerBottom) <= 0.5, `wide: the sizer ends ${wide.sizerBottom - wide.rows.at(-1).bottom}px after the last row`);
    for (const index of [0, 1, 2, 7, 8]) assert.equal(wide.rows[index].height, 19, `wide: short row ${index} is one line`);
    for (const index of [3, 4, 5, 6]) assert.ok(wide.rows[index].height >= 19 * 3, `wide: long row ${index} did not wrap (${wide.rows[index].height}px for ${wide.rows[index].length} characters)`);
    await shot("wide");

    // Narrower, each long line takes more visual lines; the rows follow.
    win.setSize(760, 620);
    await pause(200);
    const narrow = await settled("The narrow table's rows never settled");
    assertSeparate(narrow, "narrow");
    for (const index of [3, 4, 5, 6]) assert.ok(narrow.rows[index].height > wide.rows[index].height, `narrow: long row ${index} did not grow (${wide.rows[index].height} to ${narrow.rows[index].height})`);
    assert.ok(Math.abs(narrow.rows.at(-1).bottom - narrow.sizerBottom) <= 0.5, "narrow: the sizer did not follow its rows");
    await shot("narrow");

    // The second file is virtual: 440 rows (40 hunks of a header and ten lines)
    // of uneven heights, never all drawn.
    await check(() => [...document.querySelectorAll('.ah-commitview__list .ah-changerow[data-kind="file"] .ah-changerow__name')]
      .find(el => el.textContent.includes("LANDED.md")).click());
    await until(() => document.querySelector(".ah-commitview__path")?.textContent === "docs/LANDED.md"
      && document.querySelectorAll(".ah-diff .ah-code__line").length > 9, "The second file did not open");
    const top = await settled("The long diff's first rows never settled");
    assertSeparate(top, "long, top");
    assert.ok(top.rows.length < 440, "long: every row was drawn, so nothing here is virtual");
    for (const fraction of [0.37, 0.71, 1]) {
      // A hidden window runs no frames, so the scroll's own event may never be
      // sent; the virtualizer is told as the event would tell it. Twice, since
      // the first move measures new rows and the scroll height follows them.
      for (let pass = 0; pass < 2; pass++) {
        await check(value => {
          const el = document.querySelector(".ah-diff");
          el.scrollTop = (el.scrollHeight - el.clientHeight) * value;
          el.dispatchEvent(new Event("scroll"));
        }, fraction);
        await pause(120);
      }
      const seen = await settled(`The long diff never settled at ${fraction}`);
      assertSeparate(seen, `long, ${fraction}`);
      assert.ok(seen.rows[0].index > 0, `long, ${fraction}: the scroll did not move the window of rows`);
    }
    const end = await check(measure);
    assert.equal(end.rows.at(-1).index, 439, "long: the last row is not drawn at the end of the scroll");
    assert.ok(Math.abs(end.rows.at(-1).bottom - end.sizerBottom) <= 0.5, "long: the sizer does not end at its last row");
    await shot("long-end");

    assert.deepEqual(errors, [], "Renderer errors");
    console.log(JSON.stringify({ result: "PASS", screenshots, wide: wide.rows.map(row => row.height), narrow: narrow.rows.map(row => row.height),
      checked: ["rows touch and never cross", "text inside its row", "mark on the first visual line", "no sideways scroll", "sizer equals its rows", "narrow re-measure", "deep virtual scroll"] }, null, 2));
  } catch (error) { failed = true; console.error(error);
    console.error(await check(() => ({ rows: [...document.querySelectorAll(".ah-diff .ah-code__line")].map(row => [row.dataset.index, row.dataset.diff, row.textContent.length]), state: document.querySelector(".ah-fileview__state")?.textContent ?? null, body: document.body.innerText.slice(0, 300) })).catch(() => null)); if (errors.length) console.error(errors); if (screenshots.length) console.error(screenshots); }
  finally {
    clearInterval(pulse); clearTimeout(deadline);
    win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0);
  }
}
void run();
