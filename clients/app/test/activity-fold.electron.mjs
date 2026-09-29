// Disposable Electron/Vite fixture: canonical grouping, real layout and keyboard.
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-activity-"));
app.setPath("userData", path.join(temporary, "profile"));
app.on("window-all-closed", () => {});
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error("Activity check timed out"); process.exit(1); }, 90000);
let win, server, failed = false;
const errors = [];
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message) {
  for (let i = 0; i < 160; i++) { if (await check(fn)) return; await pause(25); }
  throw new Error(message);
}
const fitted = () => [...document.querySelectorAll('.tr-message__viewport')].every(view =>
  view.firstElementChild.getBoundingClientRect().height <= parseFloat(getComputedStyle(view).maxHeight) + 1);
async function screenshot(name) {
  await pause(120); // Let theme/layout changes reach the compositor.
  await fs.writeFile(path.join(temporary, name + '.png'), (await win.webContents.capturePage()).toPNG());
}
async function key(keyCode, char) {
  win.webContents.sendInputEvent({ type: 'keyDown', keyCode });
  if (char) win.webContents.sendInputEvent({ type: 'char', keyCode: char });
  win.webContents.sendInputEvent({ type: 'keyUp', keyCode });
}
async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import('vite');
    server = await createServer({ configFile: path.join(root, 'vite.config.ts'), root, logLevel: 'error',
      server: { host: '127.0.0.1', port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 960, height: 940, show: true,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    win.webContents.on('console-message', event => { if (event.level === 'error') errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/activity-fold.html`);
    win.focus(); win.webContents.focus();
    await until(() => document.querySelectorAll('.tr-activity__head').length === 3, 'three local activity groups should render');
    assert.equal(await check(() => [...document.querySelectorAll('.tr-activity__head')].every(head => head.getAttribute('aria-expanded') === 'false')), true);
    assert.equal(await check(() => document.querySelectorAll('.tr-activity__list .tr-fold').length), 0, 'no redundant tool-run fold');
    assert.equal(await check(() => [...document.querySelectorAll('.tr-reading-prompt, .tr-reading-reply')].every(node => node.getBoundingClientRect().height > 0)), true);
    assert.equal(await check(() => document.querySelector('.tr-activity__head').textContent.includes('1 failed')), true);
    assert.equal(await check(() => document.querySelector('.tr-activity[data-partial]') !== null), true);
    assert.equal(await check(() => document.querySelectorAll('.tr-activity__running').length), 1);
    await screenshot('closed-dark');

    // Keyboard opens the outer group, then the message independently.
    await check(() => { const head = document.querySelector('.tr-activity__head'); head.scrollIntoView({ block: 'center' }); head.focus({ preventScroll: true }); });
    await key('Return', '\r');
    await until(() => !document.querySelector('.tr-activity__list').hidden, 'Enter should open the group');
    assert.equal(await check(() => document.querySelector('[data-timeline-id="long-peer"]').open), false);
    await check(() => document.querySelector('[data-timeline-id="long-peer"] > summary').focus());
    await key('Space', ' ');
    await until(() => document.querySelector('[data-timeline-id="long-peer"]').open, 'Space should open the peer body');
    await until(fitted, 'peer preview should fit');
    assert.equal(await check(() => document.querySelector('[data-timeline-id="long-peer"]').textContent.includes('PEER REPORT END')), false);
    await check(() => document.querySelector('[data-timeline-id="long-peer"]').scrollIntoView({ block: 'start' }));
    await screenshot('peer-dark');
    await check(() => document.documentElement.classList.remove('dark'));
    await screenshot('peer-light');
    await check(() => document.documentElement.classList.add('dark'));

    // Full message reader, source view, keyboard focus and exact scroll return.
    const position = await check(() => {
      const open = document.querySelector('[data-timeline-id="long-peer"] .tr-message__open');
      open.scrollIntoView({ block: 'center' }); open.focus({ preventScroll: true });
      return document.querySelector('.tr-scroll').scrollTop;
    });
    await key('Return', '\r');
    await until(() => document.querySelector('.ah-md__measure')?.textContent.includes('PEER REPORT END'), 'reader must contain the complete report');
    await check(() => [...document.querySelectorAll('.ah-fileview button')].find(button => button.textContent === 'Source').click());
    await until(() => Boolean(document.querySelector('.ah-code-shell')), 'Source should open');
    await key('Escape');
    await until(() => !document.querySelector('[role=dialog]'), 'Escape should close reader');
    await until(() => document.activeElement.classList.contains('tr-message__open'), 'reader should restore trigger focus');
    assert.equal(await check(() => document.querySelector('.tr-scroll').scrollTop), position);

    // Global activity toggle only changes outer groups, never inner state.
    await check(() => document.querySelector('[title^="Activity:"]').click());
    await until(() => [...document.querySelectorAll('.tr-activity__head')].every(head => head.getAttribute('aria-expanded') === 'true'), 'show activity should reveal all lists');
    assert.equal(await check(() => document.querySelector('[data-timeline-id="short-peer"]').open), false);
    await check(() => document.querySelector('[title^="Activity:"]').click());
    await until(() => [...document.querySelectorAll('.tr-activity__head')].every(head => head.getAttribute('aria-expanded') === 'false'), 'conversation should close all lists');
    await check(() => document.querySelector('.tr-activity__head').click());
    assert.equal(await check(() => document.querySelector('[data-timeline-id="long-peer"]').open), true, 'inner state survives global toggles');

    // Prepends replace the run's first key but must preserve its disclosures.
    await check(() => window.activityFixture.prepend());
    await until(() => Boolean(document.querySelector('[data-timeline-id="earlier-peer"]')), 'earlier event should appear');
    assert.equal(await check(() => document.querySelector('.tr-activity__head').getAttribute('aria-expanded')), 'true');
    assert.equal(await check(() => document.querySelector('[data-timeline-id="long-peer"]').open), true);
    assert.equal(await check(() => Boolean(document.querySelector('.tr-activity[data-partial]'))), false);

    // Closed groups retain the visible anchor when older activity joins them.
    const groupTop = await check(async () => {
      document.querySelector('.tr-activity__head').click();
      const scroll = document.querySelector('.tr-scroll');
      scroll.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })); scroll.scrollTop = 0;
      await new Promise(requestAnimationFrame); await new Promise(requestAnimationFrame);
      return document.querySelector('.tr-activity').getBoundingClientRect().top;
    });
    await check(() => window.activityFixture.prepend());
    await until(() => Boolean(document.querySelector('[data-timeline-id="older-peer"]')), 'second page should join the closed group');
    assert.equal(await check(() => document.querySelector('.tr-activity__head').getAttribute('aria-expanded')), 'false');
    assert.ok(Math.abs(await check(() => document.querySelector('.tr-activity').getBoundingClientRect().top) - groupTop) < 1, 'paging should preserve the visible group anchor');
    await check(() => document.querySelector('.tr-activity__head').click());

    // Incoming traffic updates a closed group in place, with no scroll jump.
    const tailBefore = await check(() => {
      const scroller = document.querySelector('.tr-scroll'); scroller.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
      scroller.scrollTop = 80; return scroller.scrollTop;
    });
    await check(() => window.activityFixture.append());
    await until(() => document.querySelectorAll('.tr-activity__head')[2].textContent.includes('2 peer messages'), 'arrival must update its group count');
    assert.equal(await check(() => document.querySelectorAll('.tr-activity__head')[2].getAttribute('aria-expanded')), 'false');
    assert.equal(await check(() => document.querySelector('.tr-scroll').scrollTop), tailBefore);

    // Search opens only its path and never opens a full-message modal.
    await check(() => { document.querySelector('.tr-activity__head').click(); window.activityFixture.seek('long-peer'); });
    await until(() => document.querySelector('[data-timeline-id="long-peer"]')?.getAttribute('data-hit') === 'true', 'search should highlight the matched peer');
    await until(() => document.activeElement === document.querySelector('[data-timeline-id="long-peer"] > summary'), 'search should focus the matched disclosure');
    assert.equal(await check(() => document.querySelectorAll('.tr-activity__head[aria-expanded="true"]').length), 1);
    assert.equal(await check(() => Boolean(document.querySelector('[role=dialog]'))), false);
    assert.equal(await check(() => document.querySelector('[data-timeline-id="long-peer"] .tr-message__open').textContent), 'View match in full message');
    await check(() => document.querySelector('[data-timeline-id="missing-peer"] > summary').click());
    assert.equal(await check(() => document.querySelector('[data-timeline-id="missing-peer"]').textContent.includes('No message body recorded')), true);
    await check(() => document.querySelector('[data-timeline-id="report"]').click());
    await until(() => document.querySelector('.tm-report__detail')?.textContent.includes('documented focus path'), 'task report detail should remain available');
    await check(() => document.querySelector('[data-timeline-id="recap"] > summary').click());
    await until(() => Boolean(document.querySelector('[data-timeline-id="recap"] .tr-message__open')), 'compaction should keep long preview');

    // Layout at a genuinely narrow window in both themes.
    win.setSize(360, 900);
    await until(fitted, 'narrow previews should fit');
    assert.equal(await check(() => [...document.querySelectorAll('.tr-activity, .tr-activity-event, .tr-message__footer')].every(node => node.scrollWidth <= node.clientWidth + 2)), true, 'activity must not overflow its column');
    await check(() => document.querySelector('[data-timeline-id="long-peer"]').scrollIntoView({ block: 'start' }));
    await screenshot('peer-narrow-dark');
    await check(() => document.documentElement.classList.remove('dark'));
    await screenshot('peer-narrow-light');
    await check(() => window.activityFixture.historical());
    await until(() => !document.querySelector('.tr-activity__running'), 'historical running rows must not look live');
    win.webContents.debugger.attach('1.3');
    await win.webContents.debugger.sendCommand('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
    await until(() => matchMedia('(prefers-reduced-motion: reduce)').matches, 'reduced motion should apply');
    assert.ok(await check(() => parseFloat(getComputedStyle(document.querySelector('.tr-activity-event > summary .chev')).transitionDuration) < 0.001), 'reduced motion should remove perceptible disclosure movement');
    await check(() => window.activityFixture.deep());
    await until(() => Boolean(document.querySelector('.tr-child-link')), 'third-level work should offer a full-width transcript');
    assert.equal(await check(() => document.querySelectorAll('#deep-worker-fixture .tr-sub').length), 0, 'deep work must not open another inline subscription');
    await check(() => document.querySelector('.tr-child-link').click());
    assert.equal(await check(() => window.openedThread), 'deep-worker');
    assert.deepEqual(errors, []);
    console.log(`PASS: local grouping, two disclosures, full reader/source, keyboard, focus/scroll return, room toggle, paging anchors, live arrivals, search, missing/report/compaction bodies, themes, 360px layout, reduced motion, deep worker navigation. Screenshots: ${temporary}`);
  } catch (error) {
    failed = true; console.error(error);
    if (win) { await screenshot('failure'); console.error(await check(() => ({ active: document.activeElement?.outerHTML, groups: [...document.querySelectorAll('.tr-activity__head')].map(node => node.outerHTML), errors: document.body.textContent.slice(-1200) }))); }
    console.error(`Artifacts: ${temporary}`);
  } finally {
    clearInterval(pulse); clearTimeout(deadline); win?.destroy();
    await Promise.race([server?.close(), pause(1000)]);
    app.exit(failed ? 1 : 0);
  }
}
void run();
