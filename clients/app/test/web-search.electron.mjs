// Run with `npm run test:web-search`. Uses a disposable Electron profile and
// real React/CSS/layout; never launches a daemon or reads the user's archive.
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-web-"));
app.setPath("userData", path.join(temporary, "profile"));
// Keep Node's ESM launch awake until Electron's native ready event.
const launchPulse = setInterval(() => { }, 50);
const deadline = setTimeout(() => {
  console.error("Web activity test timed out");
  process.exit(1);
}, 90000);
let server;
let win;
let failed = false;
const errors = [];
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const check = (fn, ...args) =>
  win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message) {
  for (let i = 0; i < 160; i++) {
    if (await check(fn)) return;
    await pause(25);
  }
  throw new Error(message);
}
async function run() {
  try {
    await app.whenReady();
    clearInterval(launchPulse);
    console.log("Electron ready; starting the isolated UI fixture.");
    const { createServer } = await import("vite");
    server = await createServer({
      configFile: path.join(root, "vite.config.ts"),
      root,
      logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false },
    });
    await server.listen();
    const port = server.httpServer.address().port;
    win = new BrowserWindow({
      width: 980,
      height: 860,
      show: false,
      webPreferences: {
        backgroundThrottling: false,
        contextIsolation: true,
        nodeIntegration: false,
      },
    });
    win.webContents.on("console-message", (event) => {
      if (event.level === "error") errors.push(event.message);
    });
    win.webContents.on("render-process-gone", (_event, details) =>
      errors.push(JSON.stringify(details)),
    );
    await win.loadURL(`http://127.0.0.1:${port}/test/web-search.html`);

    await until(() => Boolean(window.webFixture && document.querySelector('#web-search')), 'fixture did not render');
    const opened = [];
    win.webContents.setWindowOpenHandler(({ url }) => { opened.push(url); return { action: 'deny' }; });
    await check(() => document.querySelector('#web-search').click());
    await until(() => document.querySelectorAll('[data-case="search"] .tr-src__row').length === 5, 'sources not rendered');
    await until(() => document.querySelectorAll('[data-case="search"] img').length >= 7, 'site icons not ready');
    const stack = await check(() => [...document.querySelectorAll('#web-search .tr-fav')].map(el => el.title));
    assert.deepEqual(stack, ['github.com', 'developer.mozilla.org', 'stackoverflow.com']);
    assert.equal(await check(() => window.webFixture.calls.filter(host => host === 'github.com').length), 1, 'per-host request deduplication');
    assert.equal(await check(() => document.querySelector('#web-search .tr-line__g img')), null, 'glyph slot must remain monoline');
    assert.equal(await check(() => document.querySelector('[data-case="search"] .tr-src li:last-child .tr-fav--globe') !== null), true);
    await check(() => document.querySelector('[data-case="search"] .tr-src__row .dom').click());
    await pause(80);
    assert.deepEqual(opened, ['https://github.com/gpuweb/gpuweb'], 'domain click must open the whole source row');
    await check(() => document.querySelector('[data-case="search"] input').click());
    assert.equal(await check(() => localStorage.getItem('ah.siteIcons.v1')), 'off');
    assert.equal(await check(() => document.querySelectorAll('[data-case="search"] img').length), 0, 'off must replace all icons');
    const before = await check(() => window.webFixture.calls.length);
    await check(() => window.webFixture.probe('off.example.org'));
    assert.equal(await check(() => window.webFixture.calls.length), before, 'off must make no requests');
    await check(() => document.querySelector('[data-case="search"] input').click());
    await until(() => document.querySelectorAll('[data-case="search"] img').length >= 7, 'reenable did not restore icons');
    await check(() => window.webFixture.probe('broken.example.org'));
    await until(() => Boolean(document.querySelector('[data-probe] .tr-fav--globe')), 'decode failure must show a globe');
    await check(() => window.webFixture.probe('pending.example.org'));
    await until(() => window.webFixture.pending.length === 1, 'delayed icon not requested');
    await check(() => { window.webFixture.enable(false); window.webFixture.enable(true); });
    await until(() => window.webFixture.pending.length === 2, 'enable must start a fresh request');
    await check(() => window.webFixture.pending[0](window.webFixture.icon('#ff0000')));
    await pause(40);
    assert.equal(await check(() => document.querySelector('[data-probe] img') !== null), false, 'stale request must not repopulate icons');
    await check(() => window.webFixture.pending[1](window.webFixture.icon('#008800')));
    await until(() => Boolean(document.querySelector('[data-probe] img')), 'fresh icon did not load');
    for (const id of ['fold', 'many', 'empty', 'missing', 'invalid', 'fetch', 'fetches', 'long-domain']) await check(id => document.querySelector(`#web-${id}`).click(), id);
    assert.match(await check(() => document.querySelector('[data-case="fold"]').textContent), /7 additional sources were not retained/);
    assert.equal(await check(() => document.querySelectorAll('[data-case="many"] .tr-src__row').length), 24);
    await check(() => document.querySelector('[data-case="many"] .tr-web-more').click());
    assert.equal(await check(() => document.querySelectorAll('[data-case="many"] .tr-src__row').length), 30, 'show all must expose every retained result');
    assert.match(await check(() => document.querySelector('[data-case="empty"]').textContent), /No sources found/);
    assert.match(await check(() => document.querySelector('[data-case="missing"]').textContent), /No source details recorded/);
    assert.equal(await check(() => document.querySelectorAll('[data-case="invalid"] a').length), 0, 'invalid schemes must not be links');
    assert.equal(await check(() => document.querySelectorAll('[data-case="fetches"] .tr-fetch').length), 2, 'folded fetch details lost');
    assert.equal(await check(() => document.querySelector('[data-case="fetch"] a').href), 'https://developer.mozilla.org/en-US/docs/Web/API/WebGPU_API');
    assert.match(await check(() => document.querySelector('#web-failed').textContent), /HTTP 404/);
    await check(() => { const el = document.querySelector('#web-missing'); el.focus(); });
    win.webContents.sendInputEvent({ type: 'keyDown', keyCode: 'Space' });
    win.webContents.sendInputEvent({ type: 'keyUp', keyCode: 'Space' });
    await until(() => document.querySelector('#web-missing').getAttribute('aria-expanded') === 'false', 'Space did not collapse detail');
    const screenshots = [];
    for (const [width, dark] of [[980, false], [660, true], [440, true]]) {
      win.setSize(width, 900);
      await check(dark => { document.documentElement.classList.toggle('dark', dark); document.documentElement.setAttribute('data-theme', dark ? 'dark' : 'light'); document.querySelector('.ah-frame').scrollTop = 0; }, dark);
      await pause(120);
      const overflow = await check(() => [...document.querySelectorAll('.tr-src,.tr-line[data-web]')].filter(el => el.scrollWidth > el.clientWidth + 1).map(el => ({ class: el.className, width: el.clientWidth, scroll: el.scrollWidth })));
      assert.deepEqual(overflow, [], `horizontal clipping at ${width}`);
      const query = await check(() => document.querySelector('#web-search .tr-line__o').getBoundingClientRect().width);
      assert.ok(query > 120, `query disappeared at ${width}: ${query}`);
      const target = await check(() => document.querySelector('#web-fetch .tr-line__o').getBoundingClientRect().width);
      assert.ok(target > 140, `page title squeezed out URL at ${width}: ${target}`);
      const screenshot = path.join(temporary, `web-${width}-${dark ? 'dark' : 'light'}.png`);
      await fs.writeFile(screenshot, (await win.webContents.capturePage()).toPNG()); screenshots.push(screenshot);
    }
    assert.deepEqual(errors, [], 'renderer errors');
    console.log(JSON.stringify({ result: 'PASS', checked: ['source rows and links', 'colored icon stack', 'globe fallback', 'per-host cache', 'preference persistence', 'preference race', 'decode failure', 'fold totals', 'all retained sources', 'empty versus missing', 'fetch details', 'keyboard', 'narrow layout'], screenshots }, null, 2));
  } catch (error) { failed = true; console.error(error); if (errors.length) console.error(errors); } finally { clearInterval(launchPulse); clearTimeout(deadline); win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0); }
}
void run();
