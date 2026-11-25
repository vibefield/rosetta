// Real layout of the worktree ledger at the column's width: one line a name,
// a bar scaled to the heaviest row, the size at the edge, and a press that
// copies the name. A hidden window and a disposable profile; the page's
// clipboard is replaced by a recorder, so the machine's own is never written.
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-worktree-ledger-"));
app.setPath("userData", path.join(temporary, "profile"));
app.on("window-all-closed", () => {});
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error("Worktree ledger check timed out"); app.exit(1); }, 60_000);
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

function measure() {
  const rail = document.querySelector("[data-rail]").getBoundingClientRect();
  const textRight = el => { const range = document.createRange(); range.selectNodeContents(el); return range.getBoundingClientRect().right; };
  return {
    rail: { left: rail.left, right: rail.right },
    sum: document.querySelector(".ah-railsplit__sum").textContent,
    pageScrollsSideways: document.documentElement.scrollWidth > document.documentElement.clientWidth,
    rows: [...document.querySelectorAll(".ah-worktree")].map(row => {
      const box = row.getBoundingClientRect();
      const label = row.querySelector(".ah-worktree__label");
      const size = row.querySelector(".ah-worktree__size");
      const track = row.querySelector(".ah-worktree__bar");
      const fill = track?.firstElementChild ?? null;
      return {
        name: label.textContent, commit: row.querySelector("code")?.textContent ?? null, size: size.textContent,
        title: row.title, open: row.hasAttribute("data-open"), gone: row.hasAttribute("data-gone"),
        left: box.left, right: box.right, height: box.height,
        labelHeight: label.getBoundingClientRect().height, labelClipped: label.scrollWidth > label.clientWidth,
        labelRight: label.getBoundingClientRect().right, sizeLeft: size.getBoundingClientRect().left,
        sizeRight: size.textContent === "" ? size.getBoundingClientRect().right : textRight(size),
        share: track === null ? null : fill.getBoundingClientRect().width / track.getBoundingClientRect().width,
      };
    }),
  };
}

function assertLaidOut(seen, where) {
  assert.equal(seen.pageScrollsSideways, false, `${where}: the page scrolls sideways`);
  for (const row of seen.rows) {
    assert.ok(row.left >= seen.rail.left - 0.5 && row.right <= seen.rail.right + 0.5, `${where}: ${row.name} leaves the column`);
    assert.ok(row.height <= 44, `${where}: ${row.name} is ${row.height}px tall, more than a name over a bar`);
    assert.ok(row.labelHeight <= 18, `${where}: ${row.name}'s name wraps (${row.labelHeight}px)`);
    assert.ok(row.sizeRight <= row.right + 0.5, `${where}: ${row.name}'s size is cut at the column's edge`);
    assert.ok(row.labelRight <= row.sizeLeft + 0.5, `${where}: ${row.name}'s name runs under its size`);
  }
}

async function run() {
  try {
    await app.whenReady();
    clearInterval(pulse);
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 520, height: 520, show: false,
      webPreferences: { contextIsolation: true, nodeIntegration: false, backgroundThrottling: false } });
    win.webContents.on("console-message", event => { if (event.level === "error") errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/worktree-ledger.html`);
    await until(() => document.querySelectorAll(".ah-worktree").length === 5, "The five worktrees were not drawn");
    await check(() => {
      window.copied = [];
      Object.defineProperty(navigator, "clipboard", { configurable: true,
        value: { writeText: async text => { if (window.refuse) throw new Error("refused"); window.copied.push(text); } } });
    });

    // Heaviest first, the one being measured after them, the gone one last.
    const first = await check(measure);
    assertLaidOut(first, "first");
    assert.deepEqual(first.rows.map(row => [row.name, row.commit, row.size]), [
      ["co/items-186-187-stack-v2-with-a-name-longer-than-the-column", null, "6.00 GB"],
      ["main", null, "2.00 GB"],
      ["wt-11a-green", "8436ffa", "1.00 GB"],
      ["wt-10bfix", "0e4919f", "…"],
      ["wt-09-probe", "22fdbad", "gone"],
    ]);
    assert.equal(first.sum, "5 · 9.00 GB+", "the sum is a floor while a row is being measured");
    assert.equal(first.rows[0].labelClipped, true, "a name longer than the column is cut with an ellipsis, on its one line");
    assert.deepEqual(first.rows.map(row => row.open), [false, true, false, false, false]);
    assert.deepEqual(first.rows.map(row => row.gone), [false, false, false, false, true]);
    for (const [at, share] of [[0, 1], [1, 2 / 6], [2, 1 / 6]]) {
      assert.ok(Math.abs(first.rows[at].share - share) < 0.02, `${first.rows[at].name}'s bar is ${first.rows[at].share} of its track, not ${share}`);
    }
    assert.equal(first.rows[3].share, null, "a row with no figure has no bar");
    assert.equal(first.rows[4].share, null, "a gone folder has no bar");
    assert.equal(first.rows[1].title, "~/Projects/field · 3 dirty · Press to copy the name");
    assert.match(first.rows[2].title, /^\/private\/tmp\/.*\/scratchpad\/wt-11a-green · Press to copy the name$/);
    assert.match(first.rows[4].title, / · its folder is gone · /);
    await shot("dark");

    // The figure arrives: the row takes its place by weight, and the sum is whole.
    await check(root => window.ledgerFixture.measured(`${root}/wt-10bfix`, 3 * 1024 ** 3),
      "/private/tmp/claude-501/-Users-test-Projects-project100-vibe-field/b3dfe562-aa76-4ad0-8b46-e6516b1a780a/scratchpad");
    const second = await check(measure);
    assertLaidOut(second, "second");
    assert.deepEqual(second.rows.map(row => row.name).slice(1), ["wt-10bfix", "main", "wt-11a-green", "wt-09-probe"]);
    assert.equal(second.sum, "5 · 12.0 GB");
    assert.ok(Math.abs(second.rows[1].share - 0.5) < 0.02);

    // One press copies the name the row shows: the branch, or the folder's own.
    await check(() => document.querySelectorAll(".ah-worktree")[1].click());
    await until(() => document.querySelector("[role=status]").textContent === "worktree name copied", "A copy was silent");
    await check(() => document.querySelectorAll(".ah-worktree")[0].click());
    await until(() => window.copied.length === 2, "The second press copied nothing");
    assert.deepEqual(await check(() => window.copied), ["wt-10bfix", "co/items-186-187-stack-v2-with-a-name-longer-than-the-column"]);
    await check(() => { window.refuse = true; document.querySelectorAll(".ah-worktree")[2].click(); });
    await until(() => document.querySelector("[role=status]").textContent === "the clipboard refused", "A refused copy was silent");
    assert.equal(await check(() => window.copied.length), 2);
    assert.equal(await check(() => { const row = document.querySelectorAll(".ah-worktree")[3]; row.focus(); return document.activeElement === row && row.tagName; }), "BUTTON", "a row takes the keyboard");

    // A host that measures nothing: git's order, no bars, no sum of bytes.
    await check(() => window.ledgerFixture.unmeasured());
    const plain = await check(measure);
    assertLaidOut(plain, "plain");
    assert.deepEqual(plain.rows.map(row => [row.name.slice(0, 12), row.size, row.share]), [
      ["main", "", null], ["co/items-186", "", null], ["wt-10bfix", "", null], ["wt-11a-green", "", null], ["wt-09-probe", "gone", null],
    ]);
    assert.equal(plain.sum, "5");

    await check(() => { window.ledgerFixture.measured("/Users/test/Projects/field", 2 * 1024 ** 3); document.documentElement.classList.remove("dark"); });
    await pause(150);
    await shot("light");

    assert.deepEqual(errors, [], "Renderer errors");
    console.log(JSON.stringify({ result: "PASS", screenshots, checked: ["order by weight", "bars to scale", "one-line names", "size inside the column", "a figure arriving", "press copies the name", "refused copy says so", "a host that measures nothing"] }, null, 2));
  } catch (error) { failed = true; console.error(error); if (errors.length) console.error(errors); if (screenshots.length) console.error(screenshots); }
  finally {
    clearInterval(pulse); clearTimeout(deadline);
    win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0);
  }
}
void run();
