// Row-scoped browsing through the real transcript and file viewer, without a daemon.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { mkdtempSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { app, BrowserWindow } from 'electron';
const root = fileURLToPath(new URL('..', import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'rosetta-image-gallery-'));
app.setPath('userData', path.join(temporary, 'profile'));
app.on('window-all-closed', () => {});
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error('Image gallery test timed out'); app.exit(1); }, 60000);
const pause = (ms = 80) => new Promise(resolve => setTimeout(resolve, ms));
let win, server, failed = false;
const errors = [];
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message, ...args) {
  for (let i = 0; i < 160; i++) { if (await check(fn, ...args)) return; await pause(25); }
  throw new Error(message);
}
async function key(keyCode, modifiers = []) {
  if (!await check(() => document.hasFocus())) {
    app.focus({ steal: true }); win.focus(); win.webContents.focus();
    await until(() => document.hasFocus(), 'Fixture did not regain native focus');
  }
  win.webContents.sendInputEvent({ type: 'keyDown', keyCode, modifiers });
  win.webContents.sendInputEvent({ type: 'keyUp', keyCode, modifiers });
  await pause();
}
async function click(selector, index = 0) {
  await check((selector, index) => {
    const node = document.querySelectorAll(selector)[index];
    node.focus({ preventScroll: true }); node.click();
  }, selector, index);
  await pause();
}
async function show(selector) {
  await check(selector => {
    const scroller = document.querySelector('.tr-scroll');
    scroller.dispatchEvent(new WheelEvent('wheel', { deltaY: -50 }));
    scroller.scrollTop += document.querySelector(selector).getBoundingClientRect().top - scroller.getBoundingClientRect().top - 90;
  }, selector); await pause(150);
}
async function close(count = 0) {
  await key('Escape');
  await until(count => document.querySelectorAll('[role="dialog"]').length === count, 'Escape did not close the expected viewer', count);
  await pause(200);
}
async function image(width) {
  await until(() => Boolean(document.querySelector('.ah-image__frame img')?.naturalWidth), 'The selected image did not load');
  assert.equal(await check(() => document.querySelector('.ah-image__frame img').naturalWidth), width);
}
async function shot(name) { await pause(180); await fs.writeFile(path.join(temporary, name + '.png'), (await win.webContents.capturePage()).toPNG()); }
const thumbs = '.ah-image-gallery__thumb';
const selected = () => document.querySelector('.ah-image-gallery__thumb[aria-current]')?.dataset.imageKey;

async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import('vite');
    server = await createServer({ configFile: path.join(root, 'test/prompt-images.vite.config.ts'), root,
      logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 1100, height: 900, show: true,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    win.webContents.on('console-message', event => { if (event.level === 'error' && !event.message.includes('503')) errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/prompt-images.html`);
    app.focus({ steal: true }); win.focus(); win.webContents.focus();
    await until(() => Boolean(window.imageFixture), 'Fixture did not load');
    await show('[data-timeline-id="gallery"]');
    await until(() => document.querySelector('[data-timeline-id="gallery"] img')?.naturalWidth > 0, 'Row thumbnails did not load');
    await click('[data-timeline-id="gallery"] .tr-att__tile', 1);
    await image(320);
    assert.equal(await check(() => document.querySelectorAll('.ah-image-gallery__thumb').length), 6);
    assert.equal(await check(selected), '2');
    assert.equal(await check(() => document.querySelector('.ah-image-gallery [role="status"]').textContent), '2 / 6');
    await check(() => { window.openGalleryDialog = document.querySelector('[role="dialog"]'); });
    await click('[aria-label="Next image"]'); await image(960);
    assert.equal(await check(selected), '3', 'Duplicate bytes retain their own slot');
    await click(thumbs, 4); await image(960);
    await click('[aria-label="View full screen"]');
    await check(() => [...document.querySelectorAll('.ah-fileview__acts button')].find(button => button.textContent === '1:1').click());
    await key('Right'); await image(320);
    assert.equal(await check(selected), '6');
    assert.ok(await check(() => document.querySelector('[role="dialog"]') === window.openGalleryDialog && window.openGalleryDialog.hasAttribute('data-fullscreen')));
    assert.equal(await check(() => document.querySelector('.ah-image').dataset.mode), 'actual');
    await key('Right');
    assert.equal(await check(selected), '6', 'Navigation stops at the row boundary');
    assert.equal(await check(() => document.querySelector('[aria-label="Next image"]').getAttribute('aria-disabled')), 'true');
    await key('Escape');
    assert.ok(await check(() => document.querySelector('[role="dialog"]') && !document.querySelector('[role="dialog"]').hasAttribute('data-fullscreen')));
    await check(() => [...document.querySelectorAll('.ah-fileview__acts button')].find(button => button.textContent === 'Fit').click());
    await key('Home'); await image(960);
    await shot('gallery-dark');
    await check(() => document.documentElement.classList.remove('dark'));
    await shot('gallery-light');
    await check(() => document.documentElement.classList.add('dark'));
    win.setContentSize(360, 720); await pause(200);
    await click(`${thumbs}[aria-current]`);
    await key('End'); await image(320);
    assert.equal(await check(() => document.activeElement.getAttribute('aria-current')), 'true', 'Keyboard selection follows the thumbnail focus');
    assert.ok(await check(() => {
      const rail = document.querySelector('.ah-image-gallery__rail'), thumb = rail.querySelector('[aria-current]');
      const r = rail.getBoundingClientRect(), t = thumb.getBoundingClientRect(), dialog = document.querySelector('[role="dialog"]');
      return rail.scrollLeft > 0 && t.left >= r.left && t.right <= r.right + 1 && dialog.scrollWidth <= dialog.clientWidth + 1;
    }), 'The selected thumbnail stays visible without widening the dialog');
    await shot('gallery-narrow');
    await close();
    assert.equal(await check(() => document.activeElement.closest('figure')?.dataset.attachmentId), '2', 'Closing returns to the original opener after navigation');
    // Restore width before comparing the transcript position: reflow itself can change the scroll offset.
    win.setContentSize(1100, 900); await pause(200);
    await show('[data-timeline-id="gallery"]');

    await show('[data-timeline-id="image-reply"]');
    await click('[data-timeline-id="image-reply"] .tr-att__tile'); await image(960);
    assert.equal(await check(() => document.querySelectorAll('.ah-image-gallery__thumb').length), 2, 'Agent replies use their own image row');
    await key('Right'); await image(320); await close();

    const activity = '.tr-activity:has([data-timeline-id="image-read"])';
    await show(activity);
    await until(() => document.querySelector('.tr-activity > .tr-attachments button.tr-att__tile'), 'Tool thumbnail did not load');
    const before = await check(async () => (await fetch('/test/image-reads')).json());
    const rowKeys = await check(selector => [...document.querySelectorAll(selector + ' > .tr-attachments figure')].map(node => node.dataset.attachmentId), activity);
    const beforeScroll = await check(() => document.querySelector('.tr-scroll').scrollTop);
    await click(activity + ' > .tr-attachments button.tr-att__tile'); await image(960);
    assert.deepEqual(await check(() => [...document.querySelectorAll('.ah-image-gallery__thumb')].map(node => node.dataset.imageKey)), rowKeys, 'Folded tool/team/peer images share precisely their outer row');
    assert.equal((await check(async () => (await fetch('/test/image-reads')).json()))['image-read:1'], before['image-read:1'], 'The selected tool image reuses its preview bytes');
    await click('[data-image-key="s1:image-peer:attachment:1"]'); await image(960);
    await key('Right'); await image(320);
    assert.equal(await check(selected), 's1:image-report:attachment:1', 'Arrows skip an unavailable peer slot without changing its identity');
    assert.ok(await check(() => document.querySelector('[data-image-key="s1:image-peer:attachment:2"]').disabled));
    await shot('gallery-mixed-activity');
    await close();
    assert.equal(await check(() => document.querySelector('.tr-scroll').scrollTop), beforeScroll, 'Gallery navigation does not move the transcript');

    await show('[data-timeline-id="missing"]');
    await click('[data-timeline-id="missing"] button.tr-att__tile'); await image(960);
    assert.equal(await check(() => document.querySelectorAll('.ah-image-gallery__thumb').length), 2);
    assert.equal(await check(() => document.querySelector('.ah-image-gallery [role="status"]').textContent), '2 / 2');
    await key('Left');
    assert.equal(await check(selected), '4'); await close();

    await show('[data-timeline-id="long"]');
    await click('[data-timeline-id="long"] .tr-message__open');
    await until(() => Boolean(document.querySelector('.ah-md .tr-att')), 'Full message did not open');
    await click('.ah-md .tr-image-link'); await image(320);
    assert.equal(await check(() => document.querySelectorAll('[role="dialog"]').length), 2);
    assert.equal(await check(() => document.querySelectorAll('.ah-image-gallery__thumb').length), 2);
    await key('Right'); await image(960); await close(1);
    assert.ok(await check(() => document.activeElement.matches('.ah-md .tr-image-link')), 'Nested gallery restores its message link');
    await close();

    await check(() => window.imageFixture.setGalleryStress());
    await show('[data-timeline-id="gallery"]');
    await click('[data-timeline-id="gallery"] .tr-att__tile'); await image(960);
    await click(thumbs, 1); await click(thumbs, 2); await image(960);
    await pause(550);
    assert.equal(await check(selected), '3');
    await image(960);
    await click(thumbs, 3);
    await until(() => Boolean(document.querySelector('.ah-fileview__retry')), 'Failed main image did not offer retry');
    assert.equal(await check(() => document.querySelectorAll('.ah-image-gallery__thumb').length), 6, 'Failure keeps gallery navigation available');
    await check(() => window.imageFixture.setFailViewer(false));
    await click('.ah-fileview__retry'); await image(320);
    await click(thumbs, 4);
    await until(() => document.querySelector('.ah-fileview__state b')?.textContent.includes('not an image'), 'Decode failure did not keep the viewer usable');
    await check(() => window.imageFixture.setBadDecode(false));
    await click('.ah-fileview__retry'); await image(960);
    await click(thumbs, 5); await image(320);
    assert.equal(await check(() => document.querySelector('.ah-fileview__title strong').textContent), 'capture');
    assert.equal(await check(() => document.querySelector('.ah-fileview__kind').textContent), 'PNG', 'Extensionless image navigation uses the recorded bytes');
    await check(() => window.imageFixture.clearSession());
    await until(() => !document.querySelector('[role="dialog"]'), 'Session change retained its gallery');
    // The living design record demonstrates the same row-scoped navigation.
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/design/transcript-record.html`);
    win.setContentSize(1100, 900); await pause(150);
    await click('[data-image-case="many"]');
    await click('#im-gallery [data-image-open="0"]');
    assert.equal(await check(() => document.querySelectorAll('#im-viewer-thumbs button').length), 6);
    await key('Right');
    assert.equal(await check(() => document.querySelector('#im-viewer-position').textContent), '2 / 6');
    await click('[data-image-scale="actual"]'); await key('Right');
    assert.equal(await check(() => document.querySelector('#im-viewer-content').dataset.scale), 'actual');
    await click('[data-image-scale="fit"]'); await key('End');
    assert.equal(await check(() => document.querySelector('#im-viewer-position').textContent), '6 / 6');
    await shot('gallery-design-record');
    await key('Escape');
    assert.equal(await check(() => document.activeElement.dataset.imageOpen), '0');
    assert.deepEqual(errors, [], 'Renderer errors');
    console.log(JSON.stringify({ result: 'PASS', screenshots: temporary, checks: ['row scope for user/agent/tool/team images', 'duplicate asset occurrences', 'thumbnail and arrow-key navigation', 'row boundaries and missing slots', 'fullscreen and scale retention', 'selected thumbnail reveal', 'narrow and light/dark layouts', 'original focus and transcript position', 'nested full-message gallery', 'slow read and retry', 'session teardown'] }, null, 2));
  } catch (error) { failed = true; console.error(error); console.error({ screenshots: temporary }); if (win) await shot('failure'); }
  finally { clearInterval(pulse); await Promise.race([server?.close(), pause(1000)]); clearTimeout(deadline); app.exit(failed ? 1 : 0); }
}
void run();
