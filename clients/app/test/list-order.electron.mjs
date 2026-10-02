// Real production lists and hooks. No daemon, archive, or existing app profile.
import assert from 'node:assert/strict';
import { mkdtempSync } from 'node:fs';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { app, BrowserWindow } from 'electron';

const root = fileURLToPath(new URL('..', import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'rosetta-list-order-'));
app.setPath('userData', path.join(temporary, 'profile'));
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error('List order test timed out'); app.exit(1); }, 90_000);
let server, win, failed = false;
const errors = [];
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, label) {
  for (let i = 0; i < 120; i++) { if (await check(fn)) return; await pause(25); }
  throw new Error(label);
}
const projects = () => [...document.querySelectorAll('.ah-project-list-item')].map(row => row.dataset.motionKey);
const sessions = () => [...document.querySelectorAll('.ah-session-list-item')].map(row => row.dataset.motionKey);
async function mutate(method, ...args) {
  await check((method, args) => window.listOrderFixture.flush(() => window.listOrderFixture[method](...args)), method, args);
}
async function load() {
  console.log('Loading ordering fixture');
  await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/list-order.html`);
  if (!win.webContents.debugger.isAttached()) win.webContents.debugger.attach('1.3');
  // Hidden macOS windows suppress focusin/out even when activeElement changes.
  // Set this on the loaded page without taking desktop focus.
  await win.webContents.debugger.sendCommand('Emulation.setFocusEmulationEnabled', { enabled: true });
  await until(() => Boolean(window.listOrderFixture?.ready), 'Fixture hooks did not mount');
  await mutate('initial');
  await until(() => document.querySelectorAll('.ah-session-list-item').length === 20, 'Initial list did not render');
  await pause(400); // Initial discovery entrances are separate from live reordering.
}
async function hover(inside) {
  await check(inside => {
    const host = document.querySelector('[data-sessions] .ah-list-view');
    host.dispatchEvent(new PointerEvent(inside ? 'pointerover' : 'pointerout', {
      bubbles: true, pointerType: 'mouse', relatedTarget: inside ? null : document.body,
    }));
  }, inside);
}

async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import('vite');
    server = await createServer({ configFile: path.join(root, 'vite.config.ts'), root, logLevel: 'error',
      server: { host: '127.0.0.1', port: 0, strictPort: false, open: false } });
    await server.listen();
    win = new BrowserWindow({ width: 1000, height: 820, show: false,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    win.webContents.on('console-message', event => { if (event.level === 'error') errors.push(event.message); });
    win.webContents.on('render-process-gone', (_event, details) => errors.push(JSON.stringify(details)));
    await load();
    console.log('Checking continuous activity and one-time promotions');
    const originalSessions = await check(sessions);
    const originalProjects = await check(projects);
    for (let i = 0; i < 30; i++) await mutate('update', ['deploy', 'sidebar', 'parser'][i % 3]);
    assert.deepEqual(await check(sessions), originalSessions);
    assert.deepEqual(await check(projects), originalProjects);
    assert.equal(await check(() => document.querySelector('[data-motion-key="parser"] strong').textContent), 'parser · updated');
    assert.equal(await check(() => [...document.querySelectorAll('.ah-motion-list [data-motion-key]')].some(row => row.getAnimations().length > 0)), false);

    await mutate('update', 'older-5');
    assert.equal((await check(sessions))[0], 'older-5');
    assert.deepEqual(await check(projects), originalProjects, 'A resumed sibling must not move its active project');
    for (const id of ['sidebar', 'older-5', 'parser']) await mutate('update', id);
    assert.equal((await check(sessions))[0], 'older-5');
    await mutate('update', 'exports');
    assert.equal((await check(projects))[0], 'tools');
    await mutate('update', 'deploy');
    assert.equal((await check(projects))[0], 'tools');
    assert.equal(await check(() => document.querySelector('[data-selected]').textContent), 'parser');

    await load();
    console.log('Checking pointer protection');
    await hover(true);
    await mutate('update', 'older-5');
    assert.deepEqual(await check(sessions), originalSessions, 'Pointer protects row positions');
    assert.equal(await check(() => document.querySelector('[data-motion-key="older-5"] strong').textContent), 'older-5 · updated');
    assert.equal(await check(() => document.querySelector('[data-sessions] [data-list-updates]').hidden), false);
    assert.deepEqual(await check(() => [...document.querySelectorAll('.ah-session-list .ah-day')].map(row => row.textContent)), ['Today', 'Activity updated']);
    await hover(false);
    await until(() => document.querySelector('.ah-session-list-item').dataset.motionKey === 'older-5', 'Leaving the list did not apply the pending order');
    assert.deepEqual(await check(() => [...document.querySelectorAll('.ah-session-list .ah-day')].map(row => row.textContent)), ['Today', 'Yesterday']);

    await load();
    console.log('Checking keyboard protection');
    await check(() => document.querySelector('[data-motion-key="sidebar"]').focus({ preventScroll: true }));
    await mutate('update', 'older-5');
    assert.deepEqual(await check(sessions), originalSessions, 'Keyboard focus protects row positions');
    assert.equal(await check(() => document.activeElement.dataset.motionKey), 'sidebar');
    await check(() => document.querySelector('[data-sessions] [data-list-updates]').focus());
    await check(() => window.listOrderFixture.flush(() => document.activeElement.click()));
    assert.equal((await check(sessions))[0], 'older-5');
    assert.equal(await check(() => document.activeElement.dataset.motionKey), 'older-5', 'Applying the order restores keyboard focus to a visible row');
    assert.equal(await check(() => document.querySelector('[data-selected]').textContent), 'parser');

    await load();
    console.log('Checking scroll anchoring and insertions');
    const before = await check(() => {
      const list = document.querySelector('.ah-session-list');
      list.scrollTop = 480;
      list.dispatchEvent(new Event('scroll', { bubbles: true }));
      const anchor = [...list.querySelectorAll('[data-motion-key]')].find(row => row.offsetTop + row.offsetHeight > list.scrollTop);
      return { top: list.scrollTop, id: anchor.dataset.motionKey, offset: anchor.offsetTop - list.scrollTop };
    });
    await mutate('update', 'older-16');
    await mutate('add', 'new-session');
    assert.deepEqual(await check(sessions), originalSessions, 'Scrolled list holds promotions and insertions');
    assert.equal(await check(() => document.querySelector('.ah-session-list').scrollTop), before.top);
    await mutate('remove', 'parser');
    assert.equal(await check(id => {
      const row = document.querySelector(`[data-motion-key="${id}"]`);
      return row.offsetTop - document.querySelector('.ah-session-list').scrollTop;
    }, before.id), before.offset, 'An authoritative deletion preserves the surviving visible anchor');
    await check(() => window.listOrderFixture.flush(() => document.querySelector('[data-sessions] [data-list-updates]').click()));
    assert.equal((await check(sessions))[0], 'new-session');
    assert.equal(await check(() => document.querySelector('.ah-session-list').scrollTop), 0);
    assert.equal(new Set(await check(sessions)).size, (await check(sessions)).length);

    await load();
    console.log('Checking batched reconnect snapshots');
    await mutate('replaceAndUpdate', 'older-5');
    // The replacement and live callbacks shared one commit, but the snapshot
    // version still prevents replay from becoming a new activity period.
    assert.equal((await check(sessions))[0], 'parser');
    assert.deepEqual(await check(projects), originalProjects);
    await mutate('update', 'older-5');
    assert.equal((await check(sessions))[0], 'parser');
    await mutate('tick', 300_000);
    assert.equal((await check(sessions))[0], 'parser', 'Quiet expiry itself does not reorder');
    await mutate('update', 'older-5', 0);
    assert.equal((await check(sessions))[0], 'older-5');

    await load();
    console.log('Checking project switching, imports and reduced motion');
    await mutate('update', 'older-5');
    await mutate('project', 'website');
    assert.deepEqual(await check(sessions), ['deploy']);
    await mutate('project', 'rosetta');
    assert.equal((await check(sessions))[0], 'older-5', 'Switching projects retains activity-period ranks');
    await mutate('add', 'old-import', new Date(2026, 0, 1).getTime());
    assert.equal((await check(sessions)).at(-1), 'old-import');

    await win.webContents.debugger.sendCommand('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
    await mutate('update', 'older-7');
    assert.equal(await check(() => [...document.querySelectorAll('[data-motion-key]')].some(row => row.getAnimations().length > 0)), false);
    for (const dark of [false, true]) {
      await check(dark => document.documentElement.classList.toggle('dark', dark), dark);
      await hover(true);
      await mutate('update', dark ? 'older-9' : 'older-8');
      await pause(350);
      assert.equal(await check(() => [...document.querySelectorAll('.ah-list-view')].every(host => {
        const notice = host.querySelector('[data-list-updates]');
        if (notice.hidden) return true;
        const a = notice.getBoundingClientRect(), b = host.getBoundingClientRect();
        return a.left >= b.left && a.right <= b.right && a.top >= b.top && a.bottom <= b.bottom;
      })), true, 'The update control fits within each column');
      await fs.writeFile(path.join(temporary, dark ? 'dark.png' : 'light.png'), (await win.webContents.capturePage()).toPNG());
      await hover(false);
    }
    assert.deepEqual(errors, []);
    console.log(`List ordering checks passed. Screenshots: ${temporary}`);
  } catch (error) { failed = true; console.error(error, ...errors); }
  finally {
    clearTimeout(deadline); clearInterval(pulse);
    win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0);
  }
}
run();
