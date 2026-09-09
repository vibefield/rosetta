// Isolated production transcript: uploads, folded activity, retry and shared viewer.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { mkdtempSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { app, BrowserWindow } from 'electron';
const root = fileURLToPath(new URL('..', import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'rosetta-image-strips-'));
app.setPath('userData', path.join(temporary, 'profile'));
app.on('window-all-closed', () => {});
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error('Image strip check timed out'); process.exit(1); }, 60000);
let win, server, failed = false;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message) {
  for (let i = 0; i < 160; i++) { if (await check(fn)) return; await pause(25); }
  throw new Error(message);
}
const activity = '.tr-ledger:has([data-timeline-id="image-read"])';
async function show(selector) {
  await check(selector => {
    const scroller = document.querySelector('.tr-scroll');
    scroller.dispatchEvent(new WheelEvent('wheel', { deltaY: -50 }));
    const rect = document.querySelector(selector).getBoundingClientRect();
    scroller.scrollTop += rect.top - scroller.getBoundingClientRect().top - 90;
  }, selector);
  await pause(100);
}
async function close(remaining = 0) {
  win.webContents.sendInputEvent({ type: 'keyDown', keyCode: 'Escape' });
  win.webContents.sendInputEvent({ type: 'keyUp', keyCode: 'Escape' });
  for (let i = 0; i < 100; i++) {
    if (await check(() => document.querySelectorAll('[role=dialog]').length) === remaining) { await pause(300); return; }
    await pause(25);
  }
  throw new Error('Viewer did not close');
}
async function capture(name) {
  await pause(140);
  await fs.writeFile(path.join(temporary, name + '.png'), (await win.webContents.capturePage()).toPNG());
}
async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import('vite');
    server = await createServer({ configFile: path.join(root, 'test/prompt-images.vite.config.ts'), root,
      logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 940, height: 1050, show: true, webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    const errors = [];
    win.webContents.on('console-message', event => { if (event.level === 'error' && !event.message.includes('503')) errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/prompt-images.html`);
    win.focus(); win.webContents.focus();
    await until(() => document.querySelectorAll('.tr-attachments').length >= 9, 'Image strips did not mount');
    assert.equal(await check(() => document.querySelectorAll('.tr-prompt .tr-attachments').length), 0);
    assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id="image-only"] .tr-prompt'))), false);
    assert.equal(await check(() => document.querySelector('[data-timeline-id="image-reply"] .tr-att figure') !== null), true);
    await pause(400);
    await check(() => document.documentElement.classList.remove('dark'));
    await show('[data-timeline-id="single"]');
    await until(() => document.querySelector('[data-timeline-id="single"] img')?.naturalWidth > 0, 'Upload did not load');
    await capture('light');
    await check(() => document.documentElement.classList.add('dark'));
    await capture('dark');

    // Numbered prompt mentions open exactly the same image as its thumbnail.
    const promptPosition = await check(() => document.querySelector('.tr-scroll').scrollTop);
    const marker = await check(() => {
      const link = document.querySelector('[data-timeline-id="single"] .tr-prompt .tr-image-link');
      return { text: link.textContent, popup: link.getAttribute('aria-haspopup'), title: link.title };
    });
    assert.deepEqual(marker, { text: '[Image #1]', popup: 'dialog', title: 'Open image · landscape.png' });
    const point = await check(() => {
      const rect = document.querySelector('[data-timeline-id="single"] .tr-image-link').getBoundingClientRect();
      return { x: Math.round(rect.x + rect.width / 2), y: Math.round(rect.y + rect.height / 2) };
    });
    win.webContents.sendInputEvent({ type: 'mouseDown', ...point, button: 'left', clickCount: 1 });
    win.webContents.sendInputEvent({ type: 'mouseUp', ...point, button: 'left', clickCount: 1 });
    await until(() => document.querySelector('.ah-image__frame img')?.naturalWidth === 960, 'Prompt image link did not open its landscape image');
    assert.equal(await check(() => document.querySelector('.ah-image__frame img').naturalHeight), 480);
    await close();
    assert.equal(await check(() => document.activeElement.matches('[data-timeline-id="single"] .tr-image-link')), true);
    assert.equal(await check(() => document.querySelector('.tr-scroll').scrollTop), promptPosition);
    win.webContents.sendInputEvent({ type: 'keyDown', keyCode: 'Return' });
    win.webContents.sendInputEvent({ type: 'char', keyCode: '\r' });
    win.webContents.sendInputEvent({ type: 'keyUp', keyCode: 'Return' });
    await until(() => document.querySelector('.ah-image__frame img')?.naturalWidth === 960, 'Prompt link did not open by keyboard');
    await close();

    await show(activity);
    assert.deepEqual(await check(selector => {
      const run = document.querySelector(selector);
      return { closed: run.querySelector('.tr-fold').getAttribute('aria-expanded'),
        hidden: getComputedStyle(run.querySelector('.tr-ladder')).display,
        count: run.querySelectorAll(':scope > .tr-attachments figure').length,
        nested: run.querySelectorAll('.tr-ladder .tr-att').length };
    }, activity), { closed: 'false', hidden: 'none', count: 6, nested: 0 });
    await until(() => document.querySelector('.tr-ledger > .tr-attachments img')?.naturalWidth > 0, 'Folded image did not load');
    const readsBefore = await check(async () => (await fetch('/test/image-reads')).json());
    assert.equal(readsBefore['image-read:0'], undefined, 'Text target must not be read as image');
    await check(selector => {
      const run = document.querySelector(selector);
      window.stripBefore = run.querySelector('.tr-att');
      run.querySelector('.tr-fold').click();
    }, activity);
    assert.equal(await check(selector => getComputedStyle(document.querySelector(selector + ' .tr-ladder')).display !== 'none', activity), true);
    assert.equal(await check(selector => document.querySelector(selector + ' .tr-att') === window.stripBefore, activity), true);
    assert.equal(await check(() => document.querySelector('[data-timeline-id="image-read"]').getAttribute('aria-expanded')), 'false');
    await check(() => document.querySelector('[data-timeline-id="image-read"]').click());
    assert.equal(await check(selector => document.querySelector(selector + ' .tr-att') === window.stripBefore, activity), true);
    await check(selector => document.querySelector(selector + ' .tr-fold').click(), activity);
    await capture('folded');

    await check(selector => document.querySelector(selector + ' button.tr-att__tile').focus({ preventScroll: true }), activity);
    const before = await check(() => ({ top: document.querySelector('.tr-scroll').scrollTop, room: document.querySelector('.ah-room').getBoundingClientRect().top }));
    win.webContents.sendInputEvent({ type: 'keyDown', keyCode: 'Return' });
    win.webContents.sendInputEvent({ type: 'char', keyCode: '\r' });
    win.webContents.sendInputEvent({ type: 'keyUp', keyCode: 'Return' });
    await until(() => document.querySelector('.ah-image__frame img')?.naturalWidth > 0, 'Tool image viewer did not load');
    await capture('viewer');
    assert.deepEqual(await check(async () => (await fetch('/test/image-reads')).json()), readsBefore, 'Viewer must reuse thumbnail bytes');
    await close();
    assert.equal(await check(() => document.activeElement.classList.contains('tr-att__tile')), true);
    assert.deepEqual(await check(() => ({ top: document.querySelector('.tr-scroll').scrollTop, room: document.querySelector('.ah-room').getBoundingClientRect().top })), before);

    win.setSize(430, 900); await pause(200);
    await show('[data-timeline-id="gallery"]');
    await check(() => document.querySelector('[data-timeline-id="gallery"] .tr-att__tile').focus({ preventScroll: true }));
    const vertical = await check(() => document.querySelector('.tr-scroll').scrollTop);
    win.webContents.sendInputEvent({ type: 'keyDown', keyCode: 'End' });
    win.webContents.sendInputEvent({ type: 'keyUp', keyCode: 'End' });
    await until(() => document.querySelector('[data-timeline-id="gallery"] .tr-att').scrollLeft > 0, 'End did not scroll the image row');
    assert.equal(await check(() => document.querySelector('.tr-scroll').scrollTop), vertical);
    assert.equal(await check(() => document.documentElement.scrollWidth <= window.innerWidth), true);
    await capture('narrow');
    await show(activity);
    await until(() => Boolean(document.querySelector('.tr-ledger .tr-att__retry')), 'Tool failure did not retain a retry slot');
    await check(() => document.querySelector('.tr-ledger .tr-att__retry').click());
    await until(() => !document.querySelector('.tr-ledger .tr-att__retry'), 'Tool retry did not recover');
    await until(() => [...document.querySelectorAll('.tr-ledger > .tr-attachments img')].every(img => img.naturalWidth > 0), 'Tool retry bytes did not decode');

    await show('[data-timeline-id="missing"]');
    await check(() => document.querySelector('[data-timeline-id="missing"] .tr-image-link').click());
    assert.equal(await check(() => document.activeElement.classList.contains('tr-att__tile--missing')), true);
    await show('[data-timeline-id="failed"]');
    await until(() => Boolean(document.querySelector('[data-timeline-id="failed"] .tr-att__retry')), 'Asset failure not shown');
    await check(() => document.querySelector('[data-timeline-id="failed"] .tr-att__retry').click());
    await until(() => document.querySelector('[data-timeline-id="failed"] img')?.naturalWidth > 0, 'Asset retry did not load');
    await show('[data-timeline-id="long"]');
    assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id="long"] > .tr-attachments'))), true);
    await check(() => { const open = document.querySelector('[data-timeline-id="long"] .tr-message__open'); open.focus({ preventScroll: true }); open.click(); });
    await until(() => Boolean(document.querySelector('.ah-md .tr-att')), 'Full message did not retain images');
    await check(() => document.querySelector('.ah-md .tr-image-link').click());
    await until(() => document.querySelectorAll('[role=dialog]').length === 2, 'Full-message image did not open nested viewer');
    await close(1);
    await close();
    assert.deepEqual(errors, []);
    console.log(JSON.stringify({ result: 'passed', artifacts: temporary, checks: ['prompt marker click and keyboard open', 'prompt and reply placement', 'both fold levels', 'no duplicate thumbnails', 'same file bytes in viewer', 'focus restoration', 'horizontal keyboard scrolling', 'narrow layout', 'asset/file retry', 'missing-image links', 'nested full-message viewer'] }));
  } catch (error) { failed = true; console.error(error); console.error('Artifacts:', temporary); if (win) await capture('failure'); }
  finally { clearTimeout(deadline); clearInterval(pulse); win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0); }
}
void run();
