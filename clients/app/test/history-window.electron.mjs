import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { app, BrowserWindow } from 'electron';
const root = fileURLToPath(new URL('..', import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'rosetta-history-'));
app.setPath('userData', path.join(temporary, 'profile'));
app.on('window-all-closed', () => {});
const pulse = setInterval(() => {}, 100);
const deadline = setTimeout(() => { console.error('History check timed out'); app.exit(1); }, 90000);
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
let win, server, failed = false;
const errors = [];
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message) { for (let i = 0; i < 240; i++) { if (await check(fn)) return; await pause(25); } throw Error(message); }
async function run() {
try {
  await app.whenReady(); clearInterval(pulse);
  const { createServer } = await import('vite');
  server = await createServer({ configFile: path.join(root, 'vite.config.ts'), root, logLevel: 'error',
    server: { host: '127.0.0.1', port: 0, strictPort: false, open: false, hmr: false, watch: null } });
  await server.listen();
  win = new BrowserWindow({ width: 850, height: 780, show: true,
    webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
  win.webContents.on('console-message', event => { if (event.level === 'error') errors.push(event.message); });
  await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/history-window.html`);
  await until(() => document.querySelector('.tr-activity__head'), 'initial fold');
  assert.equal(await check(() => document.querySelectorAll('.tr-activity__head').length), 1);
  assert.equal(await check(() => document.querySelector('.tr-activity__head').textContent.includes('Loading team activity')), true);
  await check(() => window.historyFixture.releaseTeam());
  await until(() => document.querySelector('[data-timeline-id="recent-team"]'), 'covered team message');
  assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id="old-team"]'))), false, 'no earlier team-only fold');
  assert.equal(await check(() => Boolean(document.querySelector('.tr-sub__head'))), false, 'no older worker disclosure before its surrounding chat');
  assert.equal(await check(() => document.querySelectorAll('.tr-activity > .tr-attachments').length), 1, 'images remain outside closed fold');
  await until(() => window.historyFixture.feed.earlier.kind === 'held', 'useful section prefetched and held');
  assert.equal(await check(() => window.historyFixture.feed.rows.length), 200, 'prefetch must not reveal history');
  assert.equal(await check(() => window.historyFixture.requests.length), 4, 'read through dense activity to primary speech');
  await check(() => {
    window.originalGroup = document.querySelector('.tr-activity');
    window.originalRail = window.originalGroup.querySelector('.tr-att');
    window.originalRail.scrollLeft = 200;
    window.originalScroll = window.originalRail.scrollLeft;
    const head = window.originalGroup.querySelector('.tr-activity__head'); head.focus(); head.click();
  });
  await until(() => window.historyFixture.feed.rows[0].ordinal === 201, 'opening incomplete fold loads held context');
  assert.equal(await check(() => window.originalGroup.isConnected && window.originalGroup.querySelector('.tr-att') === window.originalRail), true, 'fold and image rail retain DOM identity');
  assert.equal(await check(() => window.originalRail.scrollLeft === window.originalScroll && window.originalScroll > 0), true, 'image strip position survives prepend');
  assert.equal(await check(() => document.activeElement === window.originalGroup.querySelector('.tr-activity__head')), true, 'keyboard focus survives prepend');
  assert.equal(await check(() => window.originalGroup.querySelector('.tr-activity__head').getAttribute('aria-expanded')), 'true');
  assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id="old-team"]'))), false);

  // An out-of-window team search loads its surrounding conversation before landing.
  await check(() => window.historyFixture.seek('old-team'));
  await until(() => document.querySelector('[data-timeline-id="old-team"]')?.open, 'team search hydrates the surrounding window');
  assert.equal(await check(() => window.historyFixture.feed.rows[0].ordinal), 1);
  assert.equal(await check(() => window.historyFixture.feed.earlier.kind), 'none');
  assert.equal(await check(() => document.querySelector('.tr-sub__head')?.textContent.includes('Older standalone worker')), true);
  assert.equal(await check(() => window.originalGroup.isConnected), true);
  assert.equal(await check(() => Boolean(document.querySelector('[data-unread="true"]'))), false, 'revealed history from a previously unseen team member is not a new arrival');
  await pause(150);
  writeFileSync(path.join(temporary, 'loaded.png'), (await win.webContents.capturePage()).toPNG());

  // Fail both the approach fetch and the explicit open. Retry belongs to the fold.
  await check(() => { window.historyFixture.resetFailure(); document.querySelector('.tr-scroll').scrollTop = 0; });
  await until(() => window.historyFixture.feed.earlier.kind === 'failed', 'failed prefetch');
  await check(() => {
    const head = document.querySelector('.tr-activity__head');
    if (head.getAttribute('aria-expanded') === 'true') head.click();
  });
  await pause(50);
  await check(() => document.querySelector('.tr-activity__head').click());
  await until(() => document.querySelector('.tr-activity__list:not([hidden]) .tr-activity__loading')?.textContent.includes('Could not load earlier activity'), 'fold states the loading failure');
  await check(() => document.querySelector('.tr-activity__list:not([hidden]) .tr-activity__loading button').click());
  await until(() => window.historyFixture.feed.rows[0].ordinal === 201, 'retry loads the missing activity');
  assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id="old-team"]'))), false);
  assert.deepEqual(errors, []);
  console.log(`PASS: covered team history, bounded section prefetch, explicit fold loading/retry, search context, stable fold/image/focus on prepend. Screenshot: ${temporary}/loaded.png`);
} catch (error) {
  failed = true; console.error(error);
  if (win) {
    writeFileSync(path.join(temporary, 'failure.png'), (await win.webContents.capturePage()).toPNG());
    console.error(await check(() => ({ body: document.body.textContent.slice(0, 1800), earlier: window.historyFixture?.feed?.earlier, requests: window.historyFixture?.requests, rows: window.historyFixture?.feed?.rows?.length })));
  }
} finally {
  clearInterval(pulse); clearTimeout(deadline); win?.destroy();
  await Promise.race([server?.close(), pause(1000)]);
  app.exit(failed ? 1 : 0);
}
}
void run();
