// Real layout, focus, scrolling and Markdown rendering with a disposable profile.
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-message-preview-"));
app.setPath("userData", path.join(temporary, "profile"));
app.on("window-all-closed", () => {});
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error("Message preview check timed out"); process.exit(1); }, 60000);
let win, server;
let failed = false;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message) {
  for (let i = 0; i < 120; i++) {
    if (await check(fn)) return;
    await pause(25);
  }
  throw new Error(message);
}
const fitted = () => [...document.querySelectorAll(".tr-message__viewport")].every(view =>
  view.firstElementChild.getBoundingClientRect().height <= parseFloat(getComputedStyle(view).maxHeight) + 1);
async function open(id) {
  win.focus();
  win.webContents.focus();
  await check(id => {
    const button = document.querySelector(`[data-timeline-id="${id}"] .tr-message__open`);
    button.scrollIntoView({ block: "center" });
    button.focus({ preventScroll: true });
  }, id);
  await pause(50);
  const position = await check(() => document.querySelector('.tr-scroll').scrollTop);
  win.webContents.sendInputEvent({ type: "keyDown", keyCode: "Return" });
  win.webContents.sendInputEvent({ type: "char", keyCode: "\r" });
  win.webContents.sendInputEvent({ type: "keyUp", keyCode: "Return" });
  await until(() => Boolean(document.querySelector(".ah-md__measure")), "Markdown viewer did not open");
  await pause(280); // Let the dialog's entrance finish before capturing it.
  return position;
}
async function close() {
  win.webContents.sendInputEvent({ type: "keyDown", keyCode: "Escape" });
  win.webContents.sendInputEvent({ type: "keyUp", keyCode: "Escape" });
  await until(() => !document.querySelector("[role=dialog]"), "Escape did not close the viewer");
}

async function run() {
  try {
    await app.whenReady();
    clearInterval(pulse);
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 940, height: 980, show: true,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    const errors = [];
    win.webContents.on("console-message", event => { if (event.level === "error") errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/message-preview.html`);
    win.focus();
    await until(() => document.querySelectorAll(".tr-message__open").length === 4, "long messages did not become previews");
    await until(fitted, "preview content did not fit within its height limit");
    assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id="short-prompt"] .tr-message__open'))), false);
    assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id="short-reply"] .tr-message__open'))), false);
    assert.equal(await check(() => document.querySelector('.tr-scroll').textContent.includes("PROMPT END")), false);
    assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id="code-prompt"] .tr-code__bar button'))), false);
    assert.equal(await check(() => [...document.querySelectorAll('.tr-message__open')].every(button =>
      button.getAttribute('aria-haspopup') === 'dialog' && !button.hasAttribute('aria-expanded'))), true);

    await check(() => {
      const scroller = document.querySelector('.tr-scroll');
      scroller.dispatchEvent(new WheelEvent('wheel', { deltaY: -50 }));
      document.querySelector('[data-timeline-id="long-prompt"]').scrollIntoView({ block: 'start' });
      scroller.scrollTop = Math.max(0, scroller.scrollTop - 70);
    });
    await pause(150);
    await fs.writeFile(path.join(temporary, "dark.png"), (await win.webContents.capturePage()).toPNG());
    await check(() => document.documentElement.classList.remove('dark'));
    await pause(80);
    await fs.writeFile(path.join(temporary, "light.png"), (await win.webContents.capturePage()).toPNG());
    await check(() => document.documentElement.classList.add('dark'));
    await pause(80);

    const scrollPosition = await open('long-prompt');
    assert.equal(await check(() => document.querySelector('.ah-md__measure').textContent.includes('PROMPT END — retain every word.')), true);
    assert.equal(await check(() => document.querySelector('.ah-fileview__title').textContent.includes('User prompt.md')), true);
    await fs.writeFile(path.join(temporary, "viewer.png"), (await win.webContents.capturePage()).toPNG());
    await check(() => [...document.querySelectorAll('.ah-fileview button')].find(button => button.textContent === 'Source').click());
    await until(() => Boolean(document.querySelector('.ah-code-shell')), 'Source mode did not render');
    await close();
    assert.equal(await check(() => document.activeElement?.classList.contains('tr-message__open')), true);
    assert.equal(await check(() => document.querySelector('.tr-scroll').scrollTop), scrollPosition);
    await open('long-reply');
    assert.equal(await check(() => document.querySelector('.ah-fileview__title').textContent.includes('Agent reply.md')), true);
    assert.equal(await check(() => document.querySelector('.ah-md__measure').textContent.includes('REPLY END — 完整消息 😀.')), true);
    await close();

    // Copy still means the original message, not the shortened excerpt.
    assert.equal(await check(async () => {
      let copied = null;
      Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async text => { copied = text; } } });
      document.querySelector('[data-timeline-id="long-reply"] .tr-acts button').click();
      await Promise.resolve();
      return copied === window.messageFixture.reply;
    }), true);

    win.setSize(430, 860);
    await until(fitted, 'narrow previews did not fit');
    await pause(150);
    assert.equal(await check(() => [...document.querySelectorAll('.tr-message__footer')].every(footer => footer.scrollWidth <= footer.clientWidth + 1)), true);
    await check(() => document.querySelector('[data-timeline-id="long-prompt"]').scrollIntoView({ block: 'start' }));
    await fs.writeFile(path.join(temporary, "narrow.png"), (await win.webContents.capturePage()).toPNG());
    await open('long-prompt');
    assert.equal(await check(() => {
      const dialog = document.querySelector('.ah-fileview');
      return dialog.scrollWidth <= dialog.clientWidth + 1;
    }), true, 'the Markdown viewer must fit a narrow window');
    await close();
    win.setSize(940, 980);
    await until(fitted, 'previews did not refit after widening');
    await check(() => window.messageFixture.grow());
    await until(() => document.querySelectorAll('.tr-message__open').length === 5, 'growing reply did not become a preview');
    await until(fitted, 'growing reply exceeded the height limit');
    await check(() => window.messageFixture.shorten());
    await until(() => document.querySelectorAll('.tr-message__open').length === 4, 'shortened reply kept a stale preview action');
    await check(() => window.messageFixture.seek());
    await until(() => Boolean(document.querySelector('[data-timeline-id="long-reply"][data-hit]')), 'search did not land on the long reply');
    assert.equal(await check(() => Boolean(document.querySelector('[role=dialog]'))), false);
    await open('long-reply');
    await check(() => window.messageFixture.clearSession());
    await until(() => !document.querySelector('[role=dialog]'), 'switching sessions left the old message open');
    assert.deepEqual(errors, []);
    console.log(`PASS: prompt/reply previews, height limits, both themes, narrow layout, complete Markdown/source, keyboard focus and scroll return, full copy, stream growth, search landing, session cleanup. Screenshots: ${temporary}`);
  } catch (error) {
    failed = true;
    console.error(error);
    if (win) {
      console.error(await check(() => ({ active: document.activeElement?.outerHTML, dialog: document.querySelector('[role=dialog]')?.textContent })));
      await fs.writeFile(path.join(temporary, 'failure.png'), (await win.webContents.capturePage()).toPNG());
      console.error(`Failure screenshot: ${temporary}`);
    }
  } finally {
    clearInterval(pulse);
    clearTimeout(deadline);
    win?.destroy();
    await server?.close();
    app.exit(failed ? 1 : 0);
  }
}
void run();
