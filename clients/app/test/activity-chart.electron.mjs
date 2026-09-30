// Real pointer/keyboard/focus/layout checks; uses a disposable profile and no daemon.
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-activity-"));
const screenshots = process.env.AH_ACTIVITY_SCREENSHOTS ?? temporary;
app.setPath("userData", path.join(temporary, "profile"));
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error("Activity chart test timed out"); app.exit(1); }, 60000);
const pause = (ms = 80) => new Promise(resolve => setTimeout(resolve, ms));
let server;
let win;
let failed = false;
const errors = [];
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message) {
  for (let index = 0; index < 100; index++) {
    if (await check(fn)) return;
    await pause(30);
  }
  throw new Error(message);
}
async function point(selector) {
  return check(selector => {
    const node = document.querySelector(selector);
    node.scrollIntoView({ block: "nearest" });
    const rect = node.getBoundingClientRect();
    return { x: Math.round(rect.x + rect.width / 2), y: Math.round(rect.y + rect.height / 2) };
  }, selector);
}
async function click(selector) {
  const position = await point(selector);
  win.webContents.sendInputEvent({ type: "mouseMove", ...position });
  win.webContents.sendInputEvent({ type: "mouseDown", button: "left", clickCount: 1, ...position });
  win.webContents.sendInputEvent({ type: "mouseUp", button: "left", clickCount: 1, ...position });
  await pause();
}
async function key(keyCode, modifiers = []) {
  win.webContents.sendInputEvent({ type: "keyDown", keyCode, modifiers });
  if (keyCode === "Enter") win.webContents.sendInputEvent({ type: "char", keyCode: "\r", modifiers });
  win.webContents.sendInputEvent({ type: "keyUp", keyCode, modifiers });
  await pause();
}
const opener = ".ah-chart-launcher__button";
async function open() { await click(opener); await until(() => Boolean(document.querySelector('[role="dialog"]')), "Dialog did not open"); }
async function shot(name) { await pause(250); await fs.writeFile(path.join(screenshots, name), (await win.webContents.capturePage()).toPNG()); }

async function run() {
  try {
    await app.whenReady();
    clearInterval(pulse);
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false } });
    await server.listen();
    win = new BrowserWindow({ width: 1320, height: 900, show: false,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    win.webContents.on("console-message", event => { if (event.level === "error") errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/activity-chart.html`);
    await until(() => Boolean(window.activityFixture && document.querySelector('.ah-chart-launcher__button')), "Fixture did not render");
    await fs.mkdir(screenshots, { recursive: true });

    win.webContents.sendInputEvent({ type: "mouseMove", ...await point(opener) });
    await pause(200);
    assert.match(await check(() => getComputedStyle(document.querySelector('.ah-chart-launcher__button')).cursor), /svg.*16 16, zoom-in/);
    assert.equal(await check(() => getComputedStyle(document.querySelector('.ah-chart-launcher__hint')).opacity), "1");
    await open();
    assert.equal(await check(() => document.querySelectorAll('.ah-activity [data-day]').length), 368);
    assert.equal(await check(() => document.querySelector('.ah-activity [aria-pressed="true"][data-day]').dataset.day), "2026-09-30");
    await shot("activity-dark-53-weeks.png");

    // Native hover inspection does not silently change the selected day.
    win.webContents.sendInputEvent({ type: "mouseMove", ...await point('[data-day="2026-09-29"]') });
    await pause();
    assert.match(await check(() => document.querySelector('.ah-activity__inspection').textContent), /Sep 29, 2026/);
    assert.equal(await check(() => document.querySelector('[data-day][aria-pressed="true"]').dataset.day), "2026-09-30");
    await click('[data-day="2026-09-23"]');
    await key("Left");
    assert.equal(await check(() => document.activeElement.dataset.day), "2026-09-16");
    await key("End");
    assert.equal(await check(() => document.activeElement.dataset.day), "2026-09-30", "End skips future unavailable days");
    await key("Home");
    assert.equal(await check(() => document.activeElement.dataset.day), "2025-09-28");

    await click('[aria-label="Activity date range"] button:nth-child(1)');
    assert.equal(await check(() => document.querySelectorAll('.ah-activity [data-day]').length), 30);
    const cell = await check(() => { const rect = document.querySelector('.ah-activity [data-day]').getBoundingClientRect(); return { width: rect.width, height: rect.height }; });
    assert.ok(Math.abs(cell.width - cell.height) < 1, "Short ranges retain square calendar cells");
    await click('.ah-activity__jump');
    assert.match(await check(() => document.querySelector('.ah-activity__inspection').textContent), /Sep 3, 2026/);
    await click('[data-trend-day="2026-09-18"]');
    assert.equal(await check(() => document.querySelector('[data-day][aria-pressed="true"]').dataset.day), "2026-09-18", "Trend and calendar share day selection");
    await check(() => document.documentElement.classList.remove('dark'));
    await shot("activity-light-30-days.png");
    await click('[aria-label="View full screen"]');
    assert.ok(await check(() => document.querySelector('[role="dialog"]').hasAttribute('data-fullscreen')));
    await key("Escape");
    assert.ok(await check(() => Boolean(document.querySelector('[role="dialog"]') && !document.querySelector('[role="dialog"]').hasAttribute('data-fullscreen'))));
    await key("Escape");
    await until(() => !document.querySelector('[role="dialog"]'), "Escape did not close dialog");
    assert.ok(await check(() => document.activeElement.classList.contains('ah-chart-launcher__button')), "Focus returns to chart opener");

    // Native Tab traversal needs an actual focused OS window; hidden windows
    // can receive key handlers but do not deliver browser focus-in events.
    win.show();
    app.focus({ steal: true });
    win.focus();
    await until(() => document.hasFocus(), "Fixture window did not receive native focus");
    await check(() => document.querySelector('.ah-chart-launcher__button').focus());
    await key("Enter");
    await until(() => Boolean(document.querySelector('[role="dialog"]')), "Keyboard did not open dialog");
    await key("Tab", ["shift"]);
    assert.ok(await check(() => Boolean(document.activeElement.closest('[role="dialog"]'))), "Focus is trapped inside dialog");
    await click('[aria-label="Activity date range"] button:nth-child(2)');
    assert.equal(await check(() => document.querySelectorAll('.ah-activity [data-day]').length), 90);
    win.setContentSize(620, 860);
    await check(() => document.documentElement.classList.add('dark'));
    await pause();
    assert.ok(await check(() => { const body = document.querySelector('.ah-activity'); return body.scrollWidth <= body.clientWidth + 1; }), "Narrow layout contains horizontal calendar scrolling");
    await shot("activity-narrow-90-days.png");
    await click('[aria-label="Close activity chart"]');
    await until(() => !document.querySelector('[role="dialog"]'), "Close button did not dismiss dialog");
    await open();
    win.webContents.sendInputEvent({ type: "mouseDown", button: "left", clickCount: 1, x: 8, y: 8 });
    win.webContents.sendInputEvent({ type: "mouseUp", button: "left", clickCount: 1, x: 8, y: 8 });
    await until(() => !document.querySelector('[role="dialog"]'), "Backdrop did not dismiss dialog");

    await check(() => window.activityFixture.setMode('zero'));
    await pause();
    await open();
    assert.equal(await check(() => document.querySelector('.ah-activity__stats dd').textContent), "0");
    assert.equal(await check(() => document.querySelector('.ah-activity__jump')), null);
    await key("Escape");
    await check(() => window.activityFixture.setMode('missing'));
    await pause();
    await open();
    assert.equal(await check(() => document.querySelector('.ah-activity__stats dd').textContent), "—");
    assert.match(await check(() => document.querySelector('.ah-activity__inspection').textContent), /No recorded days/);
    await click('[aria-label="Activity date range"] button:nth-child(1)');
    assert.match(await check(() => document.querySelector('.ah-activity__daily > p').textContent), /No recorded days/);
    await check(() => window.activityFixture.switchProject());
    await until(() => !document.querySelector('[role="dialog"]'), "Project switch retained the previous project's dialog");
    await check(() => window.activityFixture.setMode('pending'));
    await pause();
    assert.ok(await check(() => document.querySelector('.ah-chart-launcher__button').disabled));
    assert.deepEqual(errors, [], "Renderer errors");
    console.log(JSON.stringify({ result: "PASS", screenshots, checks: ["fullscreen cursor", "pointer and keyboard launch", "daily hover and selection", "range accuracy", "calendar navigation", "fullscreen and Escape", "focus trap and restoration", "backdrop dismissal", "light/dark/narrow layout", "zero and missing data", "project switch", "loading state"] }, null, 2));
  } catch (error) {
    failed = true;
    console.error(error);
    if (errors.length) console.error(errors);
  } finally {
    clearInterval(pulse);
    clearTimeout(deadline);
    win?.destroy();
    await server?.close();
    app.exit(failed ? 1 : 0);
  }
}
void run();
