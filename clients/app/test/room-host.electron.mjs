// The room behind its ports: HistoryRoom mounted with a fixture host, no
// daemon and no desktop bridge, checked for what it draws and what it asks.
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { app, BrowserWindow } from 'electron';
const root = fileURLToPath(new URL('..', import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'rosetta-room-host-'));
app.setPath('userData', path.join(temporary, 'profile'));
app.on('window-all-closed', () => {});
const pulse = setInterval(() => {}, 100);
const deadline = setTimeout(() => { console.error('Room host check timed out'); app.exit(1); }, 90000);
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
let win, server;
const errors = [];
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message, ...args) {
  const truthy = `(...a) => Boolean((${fn.toString()})(...a))`;
  for (let i = 0; i < 240; i++) {
    if (await win.webContents.executeJavaScript(`(${truthy})(...${JSON.stringify(args)})`)) return;
    await pause(25);
  }
  throw Error(message);
}
const mount = name => check(n => { window.roomFixture.mount(n); return true; }, name);
const click = selector => check(s => { const el = document.querySelector(s); if (!el) throw Error(`no ${s}`); el.click(); return true; }, selector);
async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import('vite');
    server = await createServer({ configFile: path.join(root, 'vite.config.ts'), root, logLevel: 'error',
      server: { host: '127.0.0.1', port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 1440, height: 900, show: false,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    win.webContents.on('console-message', event => { if (event.level === 'error') errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/room-host.html`);
    await until(() => window.roomFixture, 'fixture ready');

    // A host that offers no search of its own kind gets neither strip nor sheet.
    await mount('plain');
    await until(() => document.querySelectorAll('.ah-project-list-item').length === 2, 'projects through the catalog port');
    assert.equal(await check(() => document.querySelector('.ah-searchbar')), null, 'no strip without the search option');
    assert.equal(await check(() => document.querySelector('[data-ah-sheet]')), null, 'no sheet without the search option');
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => document.querySelectorAll('.ah-session-list-item').length === 2, 'the project\'s sessions');
    await click('.ah-session-list-item[data-motion-key="s0"]');
    await until(() => document.querySelector('[data-timeline-id="t1"]'), 'the transcript through the session port');
    assert.deepEqual(await check(() => window.roomFixture.log.opened), ['s0'], 'one open for the session being read');
    await pause(300);
    writeFileSync(path.join(temporary, 'plain.png'), (await win.webContents.capturePage()).toPNG());

    // No host: the honest faces. No worktree to offer, a closed guest, a
    // globe for every site, and nobody asked.
    assert.equal(await check(() => document.querySelector('[aria-label="Toggle the project worktree"]')), null, 'no worktree toggle without a host');
    assert.equal(await check(() => document.querySelector('.ah-cols')?.dataset.rail), 'off', 'the guest stays closed');
    await click('.tr-activity__head');
    await until(() => document.querySelectorAll('.tr-fav').length >= 2, 'the web line\'s icons');
    assert.equal(await check(() => [...document.querySelectorAll('.tr-fav')].every(icon => icon.classList.contains('tr-fav--globe'))), true, 'a globe for every site');
    assert.deepEqual(await check(() => window.roomFixture.log.favicons), [], 'no host, no ask');

    // A host without a worktree engine: the room asks it for icons and still
    // offers no worktree.
    await mount('host');
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => document.querySelector('.ah-session-list-item[data-motion-key="s0"]'), 'sessions with a host');
    await click('.ah-session-list-item[data-motion-key="s0"]');
    await until(() => document.querySelector('.tr-activity__head'), 'transcript with a host');
    await click('.tr-activity__head');
    await until(() => document.querySelectorAll('.tr-fav img').length >= 2, 'the host\'s icons');
    assert.deepEqual(await check(() => [...window.roomFixture.log.favicons].sort()), ['developer.mozilla.org', 'github.com'], 'one ask per site, of the host');
    assert.equal(await check(() => document.querySelector('[aria-label="Toggle the project worktree"]')), null, 'no worktree engine, no toggle');

    // The search option brings the room's strip, with the host's accessory in it.
    await mount('search');
    await until(() => document.querySelector('.ah-searchbar .ah-launcher'), 'the launcher in the strip');
    assert.equal(await check(() => document.querySelector('.ah-searchbar [data-accessory]')?.textContent), 'host');

    // A catalog that cannot answer: the room's title, the host's words.
    await mount('disconnected');
    await until(() => document.querySelector('.ah-empty strong')?.textContent === 'rosettad is not answering', 'disconnected title');
    assert.equal(await check(() => document.querySelector('.ah-empty [data-host-words]')?.textContent), 'The index is resting.');

    // Consent is the host's screen when it has one, an honest line when not.
    await mount('consent');
    await until(() => document.querySelector('[data-consent]'), 'the host\'s consent screen');
    await mount('consent-default');
    await until(() => document.querySelector('.ah-empty strong')?.textContent === 'Waiting for consent', 'the room\'s own consent line');

    writeFileSync(path.join(temporary, 'room.png'), (await win.webContents.capturePage()).toPNG());
    assert.deepEqual(errors, []);
    console.log(`PASS: the room behind its ports — catalog, session, search option, unavailable words, consent. Screenshot: ${temporary}/room.png`);
  } catch (error) {
    console.error(error);
    process.exitCode = 1;
  } finally {
    clearTimeout(deadline);
    await server?.close();
    app.exit(process.exitCode ?? 0);
  }
}
run();
