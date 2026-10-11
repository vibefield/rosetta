// Real viewer, host adapter, preload and snapshot service; browser launches
// are recorded so this test never opens or changes the user's browser tabs.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow, ipcMain } from "electron";
import { createBrowserPreviewService } from "../electron/browser-preview.mjs";
import { installedBrowserIcon } from "../electron/browser-icons.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-browser-ui-"));
app.setPath("userData", path.join(temporary, "profile"));
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error("Browser viewer check timed out"); app.exit(1); }, 60_000);
let win, server, previews;
let failed = false;
let available = [];
let listFailure = false;
let launchFailure = false;
let releaseLaunch;
let delayLaunch = false;
const opened = [], defaults = [], errors = [], screenshots = [];
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message) {
  for (let i = 0; i < 160; i++) { if (await check(fn)) return; await pause(25); }
  throw new Error(message);
}
async function click(id) { await check(value => document.querySelector(`[data-browser-id="${value}"]`).click(), id); }
async function focus() { await check(() => window.dispatchEvent(new Event("focus"))); }
async function file(values) { await check(value => window.browserFixture.file(value), values); }

async function run() {
  try {
    await app.whenReady();
    clearInterval(pulse);
    available = await Promise.all([
      ["chrome", "Google Chrome", "/Applications/Google Chrome.app"],
      ["firefox", "Firefox", "/Applications/Firefox.app"],
      ["safari", "Safari", "/Applications/Safari.app"],
    ].map(async ([id, name, target]) => {
      let icon = null;
      try { icon = await installedBrowserIcon(target, id); } catch { /* fallback */ }
      return { id, name, icon };
    }));
    const initial = [...available];
    ipcMain.handle("ah-file-browsers", () => { if (listFailure) throw new Error("discovery unavailable"); return available; });
    previews = createBrowserPreviewService({ tempDirectory: temporary,
      openPath: async target => { defaults.push(target); return ""; },
      browsers: {
        resolve: async id => available.find(browser => browser.id === id),
        open: async (browser, target) => {
          opened.push({ id: browser.id, target, content: readFileSync(target, "utf8") });
          if (delayLaunch) await new Promise(resolve => { releaseLaunch = resolve; });
          return !launchFailure;
        },
      },
    });
    ipcMain.handle("ah-file-open-browser-preview", (_event, raw) => previews.open(raw));
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 1000, height: 720, show: false,
      webPreferences: { preload: path.join(root, "electron/preload.cjs"), contextIsolation: true,
        nodeIntegration: false, sandbox: false, backgroundThrottling: false } });
    win.webContents.on("console-message", event => { if (event.level === "error") errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/file-viewer-browsers.html`);
    await until(() => document.querySelectorAll('[data-browser-id]:not(:disabled)').length === 3, "Three browser icons did not become ready");
    assert.deepEqual(await check(() => [...document.querySelectorAll('[data-browser-id]')].map(el => el.getAttribute("aria-label"))), ["Open in Google Chrome", "Open in Firefox", "Open in Safari"]);
    assert.equal(await check(() => !!document.querySelector('button[aria-label="Open in browser"]')), false);
    const html = await check(() => window.browserFixture.html);
    for (const [index, id] of ["chrome", "firefox", "safari"].entries()) {
      await click(id);
      await until(() => !document.querySelector('[data-browser-id][disabled]'), "Browser stayed pending");
      assert.equal(opened[index].id, id);
      assert.equal(opened[index].content, html);
    }
    assert.equal(new Set(opened.map(entry => entry.target)).size, 1);
    assert.equal(defaults.length, 0);
    for (const theme of ["dark", "light"]) {
      await check(value => document.documentElement.classList.toggle("dark", value === "dark"), theme);
      await pause(300);
      const screenshot = path.join(temporary, `${theme}.png`);
      await writeFile(screenshot, (await win.webContents.capturePage()).toPNG()); screenshots.push(screenshot);
    }
    win.setSize(520, 620);
    await pause(100);
    assert.equal(await check(() => {
      const head = document.querySelector('.ah-fileview__head').getBoundingClientRect();
      return [...document.querySelectorAll('.ah-fileview__acts button')].every(el => {
        const rect = el.getBoundingClientRect(); return rect.left >= head.left && rect.right <= head.right && rect.bottom <= head.bottom;
      });
    }), true, "Browser actions overflow the narrow header");
    const narrow = path.join(temporary, "narrow.png");
    await writeFile(narrow, (await win.webContents.capturePage()).toPNG()); screenshots.push(narrow);

    // Keyboard users can focus every installed browser and press Enter.
    await check(() => document.querySelector('[data-browser-id="firefox"]').focus());
    win.webContents.sendInputEvent({ type: "keyDown", keyCode: "Return" });
    win.webContents.sendInputEvent({ type: "char", keyCode: "\r" });
    win.webContents.sendInputEvent({ type: "keyUp", keyCode: "Return" });
    await until(() => window.browserFixture.receipts.length === 4, "Keyboard launch did not reach the host");
    assert.equal(opened[3].id, "firefox");

    delayLaunch = true;
    await click("chrome");
    await until(() => document.querySelector('[data-browser-id="chrome"]').getAttribute('aria-busy') === 'true', "Pending launch was not shown");
    await click("chrome");
    await click("firefox");
    assert.equal(opened.length, 5, "Repeated clicks must not launch additional browsers while pending");
    releaseLaunch(); delayLaunch = false;
    await until(() => !document.querySelector('[data-browser-id][disabled]'), "Launch did not settle");
    launchFailure = true;
    await click("safari");
    await until(() => window.browserFixture.receipts.at(-1) === "could not open in Safari", "Launch failure was silent");
    launchFailure = false;

    await file({ key: "slow" });
    await until(() => document.querySelectorAll('[data-browser-id][disabled]').length === 3, "Unread bytes must disable launches");
    await check(() => window.browserFixture.releaseRead());
    await until(() => document.querySelectorAll('[data-browser-id]:not(:disabled)').length === 3, "Loaded bytes did not enable launches");
    await file({ key: "failed" });
    await until(() => document.querySelectorAll('[data-browser-id][disabled]').length === 3, "Failed reads must disable launches");
    await file({ name: "empty.htm", size: 0 });
    await until(() => document.querySelectorAll('[data-browser-id]:not(:disabled)').length === 3, "Empty HTML should still open");
    await click("chrome");
    await until(() => !document.querySelector('[data-browser-id][disabled]'), "Empty preview stayed pending");
    assert.equal(opened.at(-1).content, "");
    await file({ name: "drawing.svg" });
    await until(() => document.querySelectorAll('[data-browser-id]:not(:disabled)').length === 3, "SVG lost browser controls");
    await file({ name: "file.ts" });
    await until(() => !document.querySelector('[data-browser-id]'), "Non-browser file must have no browser icons");
    await file({ name: "page.html" });
    await until(() => document.querySelectorAll('[data-browser-id]').length === 3, "HTML controls did not return");
    await check(() => window.browserFixture.diff(true));
    await until(() => !document.querySelector('[data-browser-id]'), "Diffs must have no browser icons");
    await check(() => window.browserFixture.diff(false));
    await until(() => document.querySelectorAll('[data-browser-id]').length === 3, "File controls did not return");

    available = initial.slice(0, 1); await focus();
    await until(() => document.querySelectorAll('[data-browser-id]').length === 1, "Browser removal was not refreshed on focus");
    available = []; await focus();
    await until(() => document.querySelector('.ah-fileview__browsers').textContent.includes('No supported browsers found'), "Empty discovery must not invent a default browser");
    listFailure = true; await focus();
    await until(() => !!document.querySelector('[aria-label="Retry browser detection"]'), "Discovery failure must allow retry");
    listFailure = false; available = initial;
    await check(() => document.querySelector('[aria-label="Retry browser detection"]').click());
    await until(() => document.querySelectorAll('[data-browser-id]').length === 3, "Detection retry did not recover");

    await check(() => window.browserFixture.capability("legacy"));
    await until(() => !!document.querySelector('button[aria-label="Open in browser"]'), "Legacy hosts lost default open");
    await check(() => document.querySelector('button[aria-label="Open in browser"]').click());
    await until(() => window.browserFixture.receipts.at(-1) === "opened in your browser", "Legacy open failed");
    assert.equal(defaults.length, 1);
    await check(() => window.browserFixture.capability("none"));
    assert.equal(await check(() => !!document.querySelector('.ah-fileview__browsers, button[aria-label="Open in browser"]')), false);
    await file(null);
    await until(() => !document.querySelector('[role="dialog"]'), "Viewer did not close");
    assert.deepEqual(errors, [], "Renderer errors");
    console.log(JSON.stringify({ result: "PASS", launches: opened.length, screenshots, checked: ["browser icons", "preload IPC", "exact bytes", "keyboard", "pending clicks", "failure receipt", "loading and empty files", "SVG", "non-HTML and diff", "focus refresh", "discovery retry", "legacy hosts", "no host", "narrow layout"] }, null, 2));
  } catch (error) { failed = true; console.error(error); if (errors.length) console.error(errors); }
  finally {
    clearInterval(pulse); clearTimeout(deadline);
    previews?.dispose(); win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0);
  }
}
void run();
