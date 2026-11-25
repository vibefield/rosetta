// Real layout and behaviour of the worktree ledger where the room draws it:
// the foot of the files column's split, 280px wide, sixty worktrees.
//  · nothing is cut at the column's edge, however long a name is;
//  · only the rows on screen are asked of the host, again as the list scrolls;
//  · a row whose figure is owed shows that it is being weighed;
//  · a row keeps its place while the list is scrolled, and takes its rank at rest;
//  · a press copies the name, through the host's copy door when it has one.
// A hidden window and a disposable profile; the page's clipboard is replaced
// by a recorder, so the machine's own is never written.
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
const deadline = setTimeout(() => { console.error("Worktree ledger check timed out"); app.exit(1); }, 90_000);
let win, server;
let failed = false;
const errors = [], screenshots = [];
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message, ...args) {
  for (let i = 0; i < 240; i++) { if (await check(fn, ...args)) return; await pause(25); }
  throw new Error(message);
}
async function shot(name) {
  const file = path.join(temporary, `${name}.png`);
  await writeFile(file, (await win.webContents.capturePage()).toPNG());
  screenshots.push(file);
}
const MB = 1024 ** 2;
const lane = n => `/Users/test/lanes/item-${String(n).padStart(2, "0")}`;

function measure() {
  const rail = document.querySelector("[data-rail]").getBoundingClientRect();
  const list = document.querySelector(".ah-railfoot__list");
  const view = list.getBoundingClientRect();
  const sum = document.querySelector(".ah-railsplit__sum");
  const textRight = el => { const range = document.createRange(); range.selectNodeContents(el); return range.getBoundingClientRect().right; };
  return {
    rail: { left: rail.left, right: rail.right },
    sum: sum.textContent, sumRight: textRight(sum),
    pageScrollsSideways: document.documentElement.scrollWidth > document.documentElement.clientWidth,
    listScrollsSideways: list.scrollWidth > list.clientWidth,
    scrollTop: list.scrollTop,
    rows: [...document.querySelectorAll(".ah-worktree")].map(row => {
      const box = row.getBoundingClientRect();
      const label = row.querySelector(".ah-worktree__label");
      const size = row.querySelector(".ah-worktree__size");
      const track = row.querySelector(".ah-worktree__bar");
      const fill = track?.firstElementChild ?? null;
      const wait = row.querySelector(".ah-worktree__wait");
      return {
        path: row.dataset.worktree, name: label.textContent, commit: row.querySelector("code")?.textContent ?? null,
        size: size.textContent, title: row.title,
        open: row.hasAttribute("data-open"), gone: row.hasAttribute("data-gone"), weighing: row.hasAttribute("data-weighing"),
        onScreen: box.bottom > view.top && box.top < view.bottom,
        left: box.left, right: box.right, height: box.height,
        labelHeight: label.getBoundingClientRect().height, labelClipped: label.scrollWidth > label.clientWidth,
        labelRight: label.getBoundingClientRect().right, sizeLeft: size.getBoundingClientRect().left,
        sizeRight: size.textContent === "" ? size.getBoundingClientRect().right : textRight(size),
        track: track === null ? null : { left: track.getBoundingClientRect().left, right: track.getBoundingClientRect().right, height: track.getBoundingClientRect().height,
          animated: getComputedStyle(track).animationName !== "none" },
        wait: wait === null ? null : { width: wait.getBoundingClientRect().width, height: wait.getBoundingClientRect().height },
        share: fill === null ? null : fill.getBoundingClientRect().width / track.getBoundingClientRect().width,
      };
    }),
  };
}

function assertLaidOut(seen, where) {
  assert.equal(seen.pageScrollsSideways, false, `${where}: the page scrolls sideways`);
  assert.equal(seen.listScrollsSideways, false, `${where}: the list is wider than its column`);
  assert.ok(seen.sumRight <= seen.rail.right - 8, `${where}: the head's sum ("${seen.sum}") is cut at the column's edge`);
  for (const row of seen.rows) {
    assert.ok(row.left >= seen.rail.left - 0.5 && row.right <= seen.rail.right + 0.5, `${where}: ${row.name} leaves the column (${row.right} past ${seen.rail.right})`);
    assert.ok(row.height <= 44, `${where}: ${row.name} is ${row.height}px tall, more than a name over a bar`);
    assert.ok(row.labelHeight <= 18, `${where}: ${row.name}'s name wraps (${row.labelHeight}px)`);
    assert.ok(row.sizeRight <= row.right - 4, `${where}: ${row.name}'s size is cut at the column's edge`);
    assert.ok(row.labelRight <= row.sizeLeft + 0.5, `${where}: ${row.name}'s name runs under its size`);
    if (row.track !== null) assert.ok(Math.abs(row.track.left - seen.rows[0].track.left) < 0.5 && Math.abs(row.track.right - seen.rows[0].track.right) < 0.5, `${where}: ${row.name}'s bar does not span the row as the first row's does`);
  }
}
/** Move the list as a reader's scroll would. A hidden window runs no frames,
 * so the scroll's own event is sent by hand. */
async function scrollTo(top) {
  await check(value => { const list = document.querySelector(".ah-railfoot__list"); list.scrollTop = value; list.dispatchEvent(new Event("scroll", { bubbles: true })); }, top);
  await pause(450);
}
const order = seen => seen.rows.map(row => row.name);

async function run() {
  try {
    await app.whenReady();
    clearInterval(pulse);
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 520, height: 1000, show: false,
      webPreferences: { contextIsolation: true, nodeIntegration: false, backgroundThrottling: false } });
    win.webContents.on("console-message", event => { if (event.level === "error") errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/worktree-ledger.html`);
    await until(() => document.querySelectorAll(".ah-worktree").length === 60, "The sixty worktrees were not drawn");
    await check(() => {
      window.copied = [];
      Object.defineProperty(navigator, "clipboard", { configurable: true,
        value: { writeText: async text => { if (window.refuse) throw new Error("refused"); window.copied.push(text); } } });
    });

    // ── first paint: the two kept figures lead, heaviest first; everything else is owed
    await until(() => window.asks.length >= 1, "The rows on screen were never asked for");
    const first = await check(measure);
    assertLaidOut(first, "first");
    assert.deepEqual(first.rows.slice(0, 4).map(row => [row.name.slice(0, 26), row.commit, row.size, row.weighing]), [
      ["co/parts-186-187-stack-v2-", null, "6.00 GB", false],
      ["main", null, "2.00 GB", false],
      ["wt-10bfix", first.rows[2].commit, "", true],
      ["wt-11a-green", first.rows[3].commit, "", true],
    ]);
    assert.match(first.rows[2].commit, /^[0-9a-f]{7}$/);
    assert.equal(first.rows[0].labelClipped, true, "a name longer than the column is cut with an ellipsis, on its one line");
    assert.equal(first.rows.at(-1).name, "wt-09-probe", "a gone folder is last");
    assert.deepEqual([first.rows.at(-1).size, first.rows.at(-1).gone, first.rows.at(-1).track], ["gone", true, null]);
    assert.equal(first.sum, "60 · 8.00 GB+", "the sum is a floor while rows are owed");
    assert.ok(Math.abs(first.rows[1].share - 2 / 6) < 0.02, `main's bar is ${first.rows[1].share} of its track`);
    assert.equal(first.rows[1].open, true);
    assert.equal(first.rows[1].title, "~/Projects/field · 3 dirty · Press to copy the name");

    // ── being weighed is a state the row shows: a track and a pill, with a sheen
    for (const row of first.rows.filter(row => row.onScreen && row.weighing)) {
      assert.ok(row.track !== null && row.track.height >= 2 && row.track.animated, `${row.name} is owed a figure and shows no bar being weighed`);
      assert.ok(row.wait !== null && row.wait.width >= 20 && row.wait.height >= 5, `${row.name} is owed a figure and its size's place is blank`);
    }
    assert.ok(first.rows.filter(row => row.onScreen && row.weighing).length >= 2, "no row on screen is being weighed");

    // ── only what is on screen is asked for
    const onScreen = first.rows.filter(row => row.onScreen).map(row => row.path);
    const asked = await check(() => window.asks);
    assert.ok(asked[0].length <= 16, `the first ask named ${asked[0].length} rows of a list that shows ${onScreen.length}`);
    for (const one of onScreen.filter(p => !p.endsWith("wt-09-probe"))) assert.ok(asked[0].includes(one), `${one} is on screen and was not asked for`);
    assert.ok(!asked.flat().includes(lane(40)), "a row far below the fold was asked for");
    assert.ok(!asked.flat().some(p => p.endsWith("wt-09-probe")), "a folder that is gone was asked for");
    await shot("weighing");

    // ── the host finishes a walk: the next poll draws the figure, and at rest the row takes its rank
    await check((a, b, MB) => { window.walked.set(a, 9000 * MB); window.walked.set(b, 300 * MB); },
      first.rows[2].path, first.rows[3].path, MB);
    await until(() => [...document.querySelectorAll(".ah-worktree")].some(row => row.textContent.includes("8.79 GB")), "A finished walk's figure never arrived");
    await pause(100);
    const second = await check(measure);
    assertLaidOut(second, "second");
    assert.deepEqual(second.rows.slice(0, 4).map(row => [row.name.slice(0, 12), row.size]), [
      ["wt-10bfix", "8.79 GB"], ["co/parts-186", "6.00 GB"], ["main", "2.00 GB"], ["wt-11a-green", "300 MB"],
    ]);
    assert.ok(Math.abs(second.rows[1].share - 6144 / 9000) < 0.02);

    // ── scrolled: the rows now on screen are asked for, and a figure arriving moves nothing
    const before = (await check(() => window.asks)).length;
    await scrollTo(900);
    await until(n => window.asks.length > n, "A scroll asked for nothing", before);
    const deep = await check(measure);
    assert.ok(deep.scrollTop > 0);
    const deepOnScreen = deep.rows.filter(row => row.onScreen).map(row => row.path);
    const later = (await check(() => window.asks)).slice(before).flat();
    for (const one of deepOnScreen) assert.ok(later.includes(one) || one.endsWith("item-03"), `${one} scrolled into view and was not asked for`);
    assert.ok(later.length < 60, "a scroll asked for the whole list");
    const frozen = order(deep);
    await check((p, MB) => window.walked.set(p, 20000 * MB), deepOnScreen[2], MB);
    await until(p => document.querySelector(`[data-worktree="${p}"]`).textContent.includes("19.5 GB"), "The scrolled row's figure never arrived", deepOnScreen[2]);
    const still = await check(measure);
    assertLaidOut(still, "scrolled");
    assert.deepEqual(order(still), frozen, "a row left its place while the list was scrolled");
    await shot("scrolled");

    // ── back at the top, at rest: the heaviest row leads
    await scrollTo(0);
    await until(() => document.querySelector(".ah-worktree .ah-worktree__size").textContent === "19.5 GB", "At rest the ledger did not re-rank");
    const rested = await check(measure);
    assertLaidOut(rested, "rested");
    assert.equal(rested.rows[1].name, "wt-10bfix");

    // ── a folder the host could not read has no figure and is not asked again
    await scrollTo(0);
    const item3 = rested.rows.find(row => row.path.endsWith("item-03"));
    assert.equal(item3.weighing, false, "a row the host has no figure for is still shown as being weighed");
    assert.equal(item3.size, "");

    // ── one press copies the name the row shows
    await check(() => document.querySelectorAll(".ah-worktree")[1].click());
    await until(() => document.querySelector("[role=status]").textContent === "worktree name copied", "A copy was silent");
    assert.deepEqual(await check(() => window.copied), ["wt-10bfix"]);
    await check(() => { window.refuse = true; document.querySelectorAll(".ah-worktree")[2].click(); });
    await until(() => document.querySelector("[role=status]").textContent === "the clipboard refused", "A refused copy was silent");
    assert.equal(await check(() => { const row = document.querySelectorAll(".ah-worktree")[3]; row.focus(); const is = document.activeElement === row && row.tagName; row.blur(); return is; }), "BUTTON", "a row takes the keyboard");

    // ── a host with a copy door: the copy goes through it, never the page's clipboard
    await check(() => { window.refuse = false; window.ledgerFixture.hosted(true); });
    await until(() => document.querySelectorAll(".ah-worktree").length === 60, "The hosted list was not drawn");
    await check(() => document.querySelectorAll(".ah-worktree")[1].click());
    await until(() => window.hostCopied.length === 1 && document.querySelector("[role=status]").textContent === "worktree name copied", "The host's copy door was not used");
    assert.equal(await check(() => window.copied.length), 1, "the page's clipboard was written beside the host's door");
    await check(() => { window.hostRefuses = true; document.querySelectorAll(".ah-worktree")[2].click(); });
    await until(() => document.querySelector("[role=status]").textContent === "the clipboard refused", "The host's refusal was silent");
    await check(() => window.ledgerFixture.hosted(false));

    // ── a host that cannot answer: nothing loops, and the rows stay as they were
    await check(() => { window.hostFails = true; window.ledgerFixture.reopen(); });
    await pause(2600);
    const failing = (await check(() => window.asks)).length;
    await pause(2000);
    assert.equal((await check(() => window.asks)).length, failing, "a host that cannot answer was asked again and again");
    await check(() => { window.hostFails = false; });

    // ── a host that measures nothing: the kept figures only, no row being weighed, nothing asked
    const quiet = (await check(() => window.asks)).length;
    await check(() => window.ledgerFixture.measured(false));
    await pause(500);
    const plain = await check(measure);
    assertLaidOut(plain, "plain");
    assert.equal(plain.rows.filter(row => row.weighing).length, 0);
    assert.deepEqual(plain.rows.slice(0, 3).map(row => [row.name.slice(0, 12), row.size]), [["co/parts-186", "6.00 GB"], ["main", "2.00 GB"], ["wt-10bfix", ""]]);
    assert.equal(plain.sum, "60 · 8.00 GB");
    assert.equal((await check(() => window.asks)).length, quiet, "a host that measures nothing was asked");

    await check(() => { window.ledgerFixture.measured(true); document.documentElement.classList.remove("dark"); });
    await pause(400);
    await shot("light");

    assert.deepEqual(errors, [], "Renderer errors");
    console.log(JSON.stringify({ result: "PASS", screenshots, asks: (await check(() => window.asks)).map(one => one.length),
      checked: ["nothing cut at the column's edge", "only rows on screen asked", "a weighing state", "a figure arriving", "places kept while scrolled", "rank at rest", "no figure for an unreadable folder", "press copies the name", "a host's copy door", "a failing host is not hammered", "a host that measures nothing"] }, null, 2));
  } catch (error) { failed = true; console.error(error); if (errors.length) console.error(errors); if (screenshots.length) console.error(screenshots);
    console.error(await check(() => ({ asks: window.asks.map(one => one.length), rows: [...document.querySelectorAll(".ah-worktree")].slice(0, 8).map(row => row.textContent) })).catch(() => null)); }
  finally {
    clearInterval(pulse); clearTimeout(deadline);
    win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0);
  }
}
void run();
