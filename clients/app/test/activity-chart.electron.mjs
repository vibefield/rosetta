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
    assert.ok(Number(await check(() => getComputedStyle(document.querySelector('.ah-chart-launcher__hint')).opacity)) > .99);
    await open();
    assert.equal(await check(() => document.querySelectorAll('.ah-activity [data-day]').length), 368);
    assert.equal(await check(() => document.querySelector('[aria-label="Activity date range"]')), null, "The full history has no date-range tabs");
    assert.equal(await check(() => document.querySelector('.ah-activity [aria-pressed="true"][data-day]').dataset.day), "2026-09-30");
    await until(() => document.querySelectorAll('[data-model-line]').length === 3, "Hourly lines did not load");
    assert.ok(await check(() => [...document.querySelectorAll('[data-model-line]')].every(node => node.getAttribute('d').includes('C'))), "Model lines use smooth cubic curves");
    assert.equal(await check(() => document.querySelectorAll('[data-model-area]').length), 3, "Each model has its own gradient fill");
    assert.ok(await check(() => [...document.querySelectorAll('[data-model-area]')].every(node => {
      const id = node.getAttribute('fill').slice(5, -1);
      const gradient = document.getElementById(id);
      return gradient?.tagName === 'linearGradient' && gradient.lastElementChild.getAttribute('stop-opacity') === '0';
    })), "Gradient fills fade to transparent");
    assert.match(await check(() => getComputedStyle(document.querySelector('.ah-daychart__glow')).filter), /blur\(3px\)/);
    assert.equal(await check(() => document.querySelectorAll('[data-model-future]').length), 0, "Completed days have no future guides");
    assert.equal(await check(() => document.querySelector('.ah-activity__highlights, .ah-activity__stats, .ah-activity__breakdowns')), null, "No duplicate metric rows or breakdown charts");
    assert.equal(await check(() => document.querySelectorAll('[data-run="current"]').length), 11);
    assert.equal(await check(() => document.querySelectorAll('[data-run="best"]').length), 18);
    assert.match(await check(() => document.querySelector('[data-run-link="current"]').textContent), /11 days.*Sep 20.*Sep 30/);
    assert.match(await check(() => document.querySelector('[data-run-link="best"]').textContent), /18 days/);
    assert.match(await check(() => document.querySelector('.ah-daychart__readout').textContent), /Busiest hour.*4 PM/);
    win.show();
    app.focus({ steal: true }); win.focus();
    await until(() => document.hasFocus(), "Fixture window did not receive native focus");
    await pause(350);
    assert.ok(await check(() => document.querySelector('.ah-daychart__axis').getBoundingClientRect().bottom + 12 <= document.querySelector('.ah-activity').getBoundingClientRect().bottom), "The hourly time axis fits in the initial desktop viewport");
    assert.ok(await check(() => { const node = document.querySelector('.ah-activity'); return node.scrollHeight <= node.clientHeight + 1 && node.scrollWidth <= node.clientWidth + 1; }), "The complete desktop dialog fits without scrollbars");
    await shot("activity-dark-53-weeks.png");

    // Hover previews a calendar total without changing or fetching day details.
    const requestCount = await check(() => window.activityFixture.requests.length);
    win.webContents.sendInputEvent({ type: "mouseMove", ...await point('[data-day="2026-09-29"]') });
    await pause();
    assert.match(await check(() => document.querySelector('.ah-activity .ah-calendar-heatmap__footer').textContent), /Sep 29/);
    assert.match(await check(() => document.querySelector('#activity-day-title').textContent), /Sep 30, 2026/);
    assert.equal(await check(() => window.activityFixture.requests.length), requestCount);
    assert.equal(await check(() => document.querySelector('[data-day][aria-pressed="true"]').dataset.day), "2026-09-30");

    await click('[data-day="2026-09-23"]');
    await click('[data-day="2026-09-24"]');
    await until(() => Boolean(document.querySelector('.ah-daychart')), "Selected day chart did not update");
    await pause(550);
    assert.match(await check(() => document.querySelector('#activity-day-title').textContent), /Sep 24, 2026/, "A slow response cannot replace the selected day");
    assert.ok(await check(() => window.activityFixture.requests.find(request => request.day === '2026-09-23').signal.aborted));
    await click('[aria-label="Next recorded day"]');
    assert.match(await check(() => document.querySelector('#activity-day-title').textContent), /Sep 25, 2026/);
    await click('[aria-label="Previous recorded day"]');
    assert.match(await check(() => document.querySelector('#activity-day-title').textContent), /Sep 24, 2026/);
    await click('[data-day="2026-09-23"]');
    await key("Left");
    assert.equal(await check(() => document.activeElement.dataset.day), "2026-09-16");
    await key("End");
    assert.equal(await check(() => document.activeElement.dataset.day), "2026-09-30", "End skips future unavailable days");
    await key("Home");
    assert.equal(await check(() => document.activeElement.dataset.day), "2025-09-28");

    const cell = await check(() => { const rect = document.querySelector('.ah-activity [data-day]').getBoundingClientRect(); return { width: rect.width, height: rect.height }; });
    assert.ok(Math.abs(cell.width - cell.height) < 1, "The full history retains square calendar cells");
    await click('.ah-activity__jump');
    assert.match(await check(() => document.querySelector('#activity-day-title').textContent), /Sep 3, 2026/);
    await click('[data-day="2026-09-18"]');
    await until(() => Boolean(document.querySelector('.ah-daychart')), "Hourly plot did not load");
    assert.equal(await check(() => document.querySelector('[data-day][aria-pressed="true"]').dataset.day), "2026-09-18");
    await check(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve(true)))));
    const chartBounds = await check(() => { const rect = document.querySelector('.ah-daychart svg').getBoundingClientRect(); return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }; });
    win.webContents.sendInputEvent({ type: "mouseMove", x: Math.round(chartBounds.x + chartBounds.width * 16 / 23), y: Math.round(chartBounds.y + chartBounds.height / 2) });
    await pause();
    assert.match(await check(() => document.querySelector('.ah-daychart__readout').textContent), /4 PM – 5 PM/,
      JSON.stringify(await check(bounds => {
        const plot = document.querySelector('.ah-daychart__plot').getBoundingClientRect();
        const svg = document.querySelector('.ah-daychart svg').getBoundingClientRect();
        const hit = document.elementFromPoint(Math.round(bounds.x + bounds.width * 16 / 23), Math.round(bounds.y + bounds.height / 2));
        return { prior: bounds, plot: plot.toJSON(), svg: svg.toJSON(), hit: hit?.outerHTML.slice(0, 250) };
      }, chartBounds)));
    assert.equal(await check(() => document.querySelector('.ah-daychart svg').dataset.hour), '16');
    await check(() => document.querySelector('.ah-daychart svg').focus());
    await key('Right');
    assert.equal(await check(() => document.querySelector('.ah-daychart svg').dataset.hour), '1', "Hourly chart supports keyboard inspection");
    await click('[data-model-toggle="claude-opus-5-5"]');
    assert.equal(await check(() => document.querySelectorAll('[data-model-line]').length), 2);
    assert.equal(await check(() => document.querySelectorAll('[data-model-area], .ah-daychart__glow').length), 4, "Muting removes the model's fill and glow together");
    await click('[data-model-toggle="claude-opus-5-5"]');
    assert.equal(await check(() => document.querySelectorAll('[data-model-line]').length), 3);
    await click('[data-run-link="best"]');
    assert.equal(await check(() => document.querySelectorAll('.ah-activity [data-day]').length), 368, "Older runs remain available in the full history");
    assert.match(await check(() => document.querySelector('#activity-day-title').textContent), /Jul 22, 2026/);
    await click('[data-day="2026-09-18"]');
    await until(() => Boolean(document.querySelector('.ah-daychart')), "Hourly plot did not reload");
    await check(() => document.documentElement.classList.remove('dark'));
    await pause(500);
    await check(() => { document.querySelector('.ah-activity').scrollTop = 0; });
    assert.ok(await check(() => document.querySelector('.ah-daychart__axis').getBoundingClientRect().bottom + 12 <= document.querySelector('.ah-activity').getBoundingClientRect().bottom), "The hourly time axis fits with the full history");
    assert.ok(await check(() => { const node = document.querySelector('.ah-activity'); return node.scrollHeight <= node.clientHeight + 1 && node.scrollWidth <= node.clientWidth + 1; }), "The full-history dialog fits without scrollbars");
    await shot("activity-light-history.png");
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
    await until(() => Boolean(document.querySelector('.ah-daychart')), 'Hourly data did not load after keyboard launch');
    assert.equal(await check(() => document.querySelectorAll('.ah-activity [data-day]').length), 368);
    win.setContentSize(620, 860);
    await check(() => document.documentElement.classList.add('dark'));
    await pause();
    assert.ok(await check(() => { const body = document.querySelector('.ah-activity'); return body.scrollWidth <= body.clientWidth + 1; }), "Narrow calendar paging avoids horizontal scrolling");
    assert.ok(await check(() => { const node = document.querySelector('.ah-activity'); return node.scrollHeight <= node.clientHeight + 1 && node.scrollWidth <= node.clientWidth + 1; }), "The narrow dialog fits without scrollbars");
    await shot("activity-narrow-history.png");
    assert.ok(await check(() => Boolean(document.querySelector('[aria-label="Earlier calendar weeks"]'))), "Narrow yearly calendar pages keep cells readable");
    const currentDay = await check(() => document.querySelector('#activity-day-title').textContent);
    const requestsBeforePage = await check(() => window.activityFixture.requests.length);
    await click('[aria-label="Earlier calendar weeks"]');
    assert.equal(await check(() => document.querySelector('#activity-day-title').textContent), currentDay, "Browsing calendar pages retains the selected day");
    assert.equal(await check(() => window.activityFixture.requests.length), requestsBeforePage, "Browsing calendar pages does not fetch hourly data");
    await click('.ah-activity [data-day]');
    await until(() => Boolean(document.querySelector('.ah-daychart')), 'Earlier day did not load');
    assert.notEqual(await check(() => document.querySelector('#activity-day-title').textContent), currentDay);
    await click('[aria-label="Later calendar weeks"]');
    await click('[data-day="2026-09-30"]');
    await until(() => Boolean(document.querySelector('.ah-daychart')), 'Selected day did not load');
    for (const [width, height] of [[1024, 700], [620, 700], [420, 700]]) {
      win.setContentSize(width, height);
      await pause(200);
      const dimensions = await check(() => {
        const node = document.querySelector('.ah-activity');
        const calendar = document.querySelector('.ah-activity__calendar-scroll');
        const plot = document.querySelector('.ah-daychart svg').getBoundingClientRect();
        return { height: node.clientHeight, scrollHeight: node.scrollHeight,
          width: node.clientWidth, scrollWidth: node.scrollWidth,
          calendarWidth: calendar.clientWidth, calendarScrollWidth: calendar.scrollWidth,
          plotHeight: plot.height };
      });
      assert.ok(dimensions.scrollHeight <= dimensions.height + 1 && dimensions.scrollWidth <= dimensions.width + 1,
        `The entire ${width}×${height} dialog fits: ${JSON.stringify(dimensions)}`);
      assert.ok(dimensions.calendarScrollWidth <= dimensions.calendarWidth + 1, 'Calendar pages avoid horizontal scrolling');
      assert.ok(dimensions.plotHeight >= 80, 'The compact plot stays readable');
      await shot(`activity-compact-${width}.png`);
    }
    await click('[aria-label="Close activity chart"]');
    await until(() => !document.querySelector('[role="dialog"]'), "Close button did not dismiss dialog");

    // The current day ends at the current hour. Future guides have no fill or
    // numeric readout; keyboard inspection stays within recorded hours.
    win.setContentSize(1024, 700);
    await check(() => window.activityFixture.setHour(17));
    await pause();
    await open();
    await until(() => document.querySelectorAll('[data-model-future]').length === 3, "Current day did not show future guides");
    const futureGeometry = await check(() => [...document.querySelectorAll('[data-model-future]')].map(node => {
      const line = document.querySelector(`[data-model-line="${node.dataset.modelFuture}"]`);
      return { end: line.getPointAtLength(line.getTotalLength()).x,
        guideStart: node.getPointAtLength(0).x, guideEnd: node.getPointAtLength(node.getTotalLength()).x,
        dash: getComputedStyle(node).strokeDasharray, fill: getComputedStyle(node).fill };
    }));
    for (const guide of futureGeometry) {
      assert.ok(Math.abs(guide.end - 17 / 23 * 1000) < .01);
      assert.ok(Math.abs(guide.guideStart - guide.end) < .01, "Future guides begin at the last recorded point");
      assert.equal(guide.guideEnd, 1000);
      assert.notEqual(guide.dash, 'none');
      assert.equal(guide.fill, 'none');
    }
    await shot("activity-curves-future.png");
    const futureBounds = await check(() => {
      const rect = document.querySelector('.ah-daychart svg').getBoundingClientRect();
      return { x: Math.round(rect.x + rect.width * 20 / 23), y: Math.round(rect.y + rect.height / 2) };
    });
    win.webContents.sendInputEvent({ type: 'mouseMove', ...futureBounds });
    await pause();
    assert.match(await check(() => document.querySelector('.ah-daychart__readout').textContent), /8 PM – 9 PM.*Awaiting usage/);
    assert.ok(await check(() => [...document.querySelectorAll('[data-model-toggle] b')].every(node => node.textContent === '—')), "Future hours cannot appear as measured zeros");
    assert.equal(await check(() => document.querySelectorAll('.ah-daychart__point').length), 0, "Future inspection has no measured markers");
    await check(() => document.querySelector('.ah-daychart svg').focus());
    await key('End');
    assert.equal(await check(() => document.querySelector('.ah-daychart svg').dataset.hour), '17');
    await key('Right');
    assert.equal(await check(() => document.querySelector('.ah-daychart svg').dataset.hour), '17');
    await click('[data-model-toggle="claude-opus-5-5"]');
    assert.equal(await check(() => document.querySelectorAll('[data-model-future]').length), 2, "Future guides respect model isolation");
    await click('[data-model-toggle="claude-opus-5-5"]');
    await check(() => document.documentElement.classList.remove('dark'));
    await shot("activity-curves-light.png");
    await check(() => document.documentElement.classList.add('dark'));
    win.setContentSize(420, 700);
    await pause(200);
    assert.ok(await check(() => {
      const node = document.querySelector('.ah-activity');
      return node.scrollHeight <= node.clientHeight + 1 && node.scrollWidth <= node.clientWidth + 1;
    }), "Curves, glow and future label retain the compact layout");
    await shot("activity-curves-narrow.png");
    await key('Escape');
    await check(() => window.activityFixture.setHour(23));
    await pause();
    await open();
    win.webContents.sendInputEvent({ type: "mouseDown", button: "left", clickCount: 1, x: 8, y: 8 });
    win.webContents.sendInputEvent({ type: "mouseUp", button: "left", clickCount: 1, x: 8, y: 8 });
    await until(() => !document.querySelector('[role="dialog"]'), "Backdrop did not dismiss dialog");

    await check(() => window.activityFixture.setMode('error'));
    await pause();
    await open();
    await until(() => Boolean(document.querySelector('.ah-activity__empty button')), "API error did not offer retry");
    assert.equal(await check(() => document.querySelector('.ah-daychart')), null);
    await check(() => window.activityFixture.setMode('ready'));
    await click('.ah-activity__empty button');
    await until(() => Boolean(document.querySelector('.ah-daychart')), "Retry did not restore hourly data");
    await key("Escape");

    await check(() => window.activityFixture.setMode('no-usage'));
    await pause();
    await open();
    await until(() => document.querySelector('.ah-activity__empty')?.textContent.includes('Hourly breakdown unavailable'), "Missing hourly data was presented as zero");
    await key("Escape");
    await check(() => window.activityFixture.setMode('zero'));
    await pause();
    await open();
    await until(() => document.querySelector('.ah-activity__empty')?.textContent.includes('No token activity'), "Recorded zero day did not explain empty activity");
    assert.equal(await check(() => document.querySelector('.ah-activity__jump, .ah-activity__run')), null);
    await key("Escape");
    await check(() => window.activityFixture.setMode('missing'));
    await pause();
    const beforeMissing = await check(() => window.activityFixture.requests.length);
    await open();
    assert.match(await check(() => document.querySelector('#activity-day-title').textContent), /No recorded days/);
    assert.equal(await check(() => window.activityFixture.requests.length), beforeMissing, "Unavailable calendar days do not fetch fabricated usage");
    await check(() => window.activityFixture.switchProject());
    await until(() => !document.querySelector('[role="dialog"]'), "Project switch retained the previous project's dialog");
    await check(() => window.activityFixture.setMode('pending'));
    await pause();
    assert.ok(await check(() => document.querySelector('.ah-chart-launcher__button').disabled));
    assert.deepEqual(errors, [], "Renderer errors");
    console.log(JSON.stringify({ result: "PASS", screenshots, checks: ["fullscreen cursor", "pointer and keyboard launch", "hover does not fetch", "selected-day hourly models", "smooth curves and gradient glow", "dashed future guides without measured values", "stale response cancellation", "model isolation", "API error and retry", "full activity history without range tabs", "calendar navigation", "fullscreen and Escape", "focus trap and restoration", "backdrop dismissal", "light/dark/narrow layout", "no dialog scrollbars", "calendar page navigation", "zero and missing data", "project switch", "loading state"] }, null, 2));
  } catch (error) {
    failed = true;
    console.error(error);
    if (win) { await shot("activity-failure.png"); console.error({ screenshots }); }
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
