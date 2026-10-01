// Delayed snapshots, real layout and keyboard behavior, isolated from user data.
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { app, BrowserWindow } from 'electron';

const root = fileURLToPath(new URL('..', import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'rosetta-transcript-loading-'));
app.setPath('userData', path.join(temporary, 'profile'));
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error('Session opening test timed out'); process.exit(1); }, 90000);
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
let server, win, failed = false;
const errors = [];
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message) {
  for (let i = 0; i < 160; i++) { if (await check(fn)) return; await pause(25); }
  throw Error(message);
}
const opened = () => document.querySelector('.tr-stage')?.hasAttribute('data-ready');
const loading = () => Boolean(document.querySelector('.tr-loading'));
const atBottom = () => {
  const s = document.querySelector('.tr-scroll');
  return Math.abs(s.scrollHeight - s.clientHeight - s.scrollTop) < 2;
};
async function capture(name) {
  // Give the compositor the completed fade/theme frame, not its previous buffer.
  await pause(300);
  writeFileSync(path.join(temporary, `${name}.png`), (await win.webContents.capturePage()).toPNG());
}

async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import('vite');
    server = await createServer({ configFile: path.join(root, 'vite.config.ts'), root, logLevel: 'error',
      server: { host: '127.0.0.1', port: 0, strictPort: false, open: false } });
    await server.listen();
    win = new BrowserWindow({ width: 1120, height: 800, show: false,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    win.webContents.on('console-message', event => { if (event.level === 'error') errors.push(event.message); });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/transcript-loading.html`);
    await until(() => Boolean(window.loadingFixture && document.querySelector('.tr-loading')), 'initial loader missing');
    await pause(350);
    const initial = await check(() => ({ busy: document.querySelector('.tr-scroll').getAttribute('aria-busy'),
      inert: document.querySelector('.tr-surface').inert, count: document.querySelector('.ah-col--measure .ah-bubble'),
      empty: document.querySelector('.tr-surface .ah-empty'), title: document.querySelector('.ah-colhead strong').textContent,
      opacity: getComputedStyle(document.querySelector('.tr-surface')).opacity }));
    assert.deepEqual(initial, { busy: 'true', inert: true, count: null, empty: null,
      title: 'Refine the reading experience', opacity: '0' });
    await capture('loading-light');

    // First visible frame must already be at the final foot, including the
    // long message's measured preview and all collapsed activity.
    const frames = await check(async () => {
      window.loadingFixture.release('alpha');
      const samples = [];
      for (let i = 0; i < 22; i++) {
        await new Promise(requestAnimationFrame);
        const s = document.querySelector('.tr-scroll');
        if (document.querySelector('.tr-stage').hasAttribute('data-ready')) samples.push({
          gap: s.scrollHeight - s.clientHeight - s.scrollTop, top: s.scrollTop, height: s.scrollHeight,
          last: document.querySelector('[data-timeline-id="alpha-23"]').getBoundingClientRect().top,
        });
      }
      return samples;
    });
    assert.ok(frames.length > 5, 'document must reveal without an artificial loading delay');
    assert.ok(frames.every(frame => Math.abs(frame.gap) < 2), JSON.stringify(frames));
    assert.ok(frames.every(frame => Math.abs(frame.last - frames[0].last) < 1), 'revealed layout must remain steady');
    assert.equal(await check(() => document.querySelector('.tr-surface').inert), false);
    await capture('ready-light');

    await check(() => window.loadingFixture.reconnect('alpha'));
    assert.equal(await check(loading), false, 'reconnect keeps readable content');
    await check(() => window.loadingFixture.select('beta'));
    await until(loading, 'switch did not cover previous session');
    assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id^="alpha-"]'))), false);
    await check(() => {
      const stale = window.loadingFixture.subscriptions[0];
      stale.handlers.onChange(stale.rows);
      stale.handlers.onError({ code: 'internal', message: 'Stale failure' });
    });
    assert.equal(await check(() => window.loadingFixture.feed.snapshotReady), false);
    await check(() => window.loadingFixture.release('beta'));
    await until(opened, 'second session did not open');
    assert.equal(await check(atBottom), true);

    await check(() => window.loadingFixture.select('team'));
    await until(() => window.loadingFixture.teams.has('worker'), 'team subscription missing');
    await check(() => window.loadingFixture.release('team'));
    await until(() => document.querySelector('.tr-loading')?.textContent.includes('Loading team messages'), 'team wait missing');
    assert.equal(await check(() => document.querySelector('.tr-surface').inert), true);
    await check(() => window.loadingFixture.releaseTeam());
    await until(opened, 'complete team history did not open');
    assert.equal(await check(atBottom), true);
    assert.equal(await check(() => Boolean(document.querySelector('[data-timeline-id="worker-report"]'))), true);
    await check(() => window.loadingFixture.addTeam());
    await until(() => window.loadingFixture.teams.has('second-worker'), 'new team subscription missing');
    assert.equal(await check(loading), false, 'roster growth must not cover an already open document');
    await check(() => window.loadingFixture.releaseTeam('second-worker'));

    // Reopening has a fresh team read. The user can continue immediately,
    // without losing keyboard focus when the loading action disappears.
    await check(() => window.loadingFixture.select('beta'));
    await until(loading, 'reopen missing');
    await check(() => window.loadingFixture.select('team'));
    await until(() => !window.loadingFixture.feed.snapshotReady, 'new team session did not reset');
    await check(() => window.loadingFixture.release('team'));
    await until(() => Boolean(document.querySelector('.tr-loading button')), 'continue action missing');
    await check(() => { const button = document.querySelector('.tr-loading button'); button.focus(); button.click(); });
    await until(opened, 'continue did not open parent messages');
    assert.equal(await check(() => document.activeElement.classList.contains('tr-scroll')), true);
    assert.equal(await check(atBottom), true);

    await check(() => {
      window.loadingFixture.teams.get('worker').onError({ code: 'internal', message: 'Team offline' });
      window.loadingFixture.releaseTeam('second-worker');
    });
    assert.equal(await check(loading), false, 'a team failure stays inline after continuing');
    await check(() => document.querySelector('.tm-known .tr-line').click());
    await until(() => document.querySelector('.tm-known')?.textContent.includes('Could not load team messages'), 'roster needs a retry even without a team event');
    assert.equal(await check(() => Boolean(document.querySelector('.tm-known .tr-activity__loading button'))), true);

    await check(() => window.loadingFixture.select('empty'));
    await until(loading, 'empty session skipped loading');
    assert.equal(await check(() => Boolean(document.querySelector('.tr-surface .ah-empty'))), false);
    await check(() => window.loadingFixture.release('empty'));
    await until(opened, 'empty snapshot did not settle');
    assert.equal(await check(() => document.querySelector('.tr-surface .ah-empty strong').textContent), 'Nothing readable yet');

    await check(() => window.loadingFixture.select('failure'));
    await until(loading, 'failure session loader missing');
    await check(() => window.loadingFixture.fail('failure'));
    await until(() => document.querySelector('.tr-loading')?.hasAttribute('data-error'), 'failure stayed a spinner');
    await capture('error-light');
    const attempts = await check(() => window.loadingFixture.subscriptions.length);
    await check(() => document.querySelector('.tr-loading button').click());
    await until(() => !document.querySelector('.tr-loading')?.hasAttribute('data-error'), 'retry did not clear failure');
    assert.equal(await check(() => window.loadingFixture.subscriptions.length), attempts + 1);
    await check(() => window.loadingFixture.release('failure'));
    await until(opened, 'retry failed to open');

    await check(() => window.loadingFixture.select('alpha', true));
    await until(loading, 'search selection missing');
    await check(() => window.loadingFixture.subscriptions.at(-1).handlers.onStatus('live'));
    await pause(60);
    assert.equal(await check(() => window.loadingFixture.subscriptions.at(-1).anchorReads), 0,
      'a live status before the first snapshot must not consume the search target');
    await check(() => window.loadingFixture.release('alpha'));
    await until(opened, 'search anchor did not settle');
    assert.equal(await check(atBottom), false, 'search landing must not jump to the latest message');
    assert.equal(await check(() => {
      const s = document.querySelector('.tr-scroll').getBoundingClientRect();
      const target = document.querySelector('[data-timeline-id="alpha-3"]').getBoundingClientRect();
      return target.bottom > s.top && target.top < s.bottom;
    }), true);

    // Rapid A/B/A selection must reject the first A's delayed callbacks.
    await check(() => window.loadingFixture.select('beta'));
    await until(loading, 'rapid switch B missing');
    await check(() => window.loadingFixture.select('alpha'));
    await until(loading, 'rapid switch A missing');
    await check(() => window.loadingFixture.subscriptions[0].handlers.onChange(window.loadingFixture.subscriptions[0].rows));
    assert.equal(await check(() => window.loadingFixture.feed.snapshotReady), false);
    await check(() => window.loadingFixture.release('alpha'));
    await until(opened, 'rapid switch did not recover');
    await check(() => window.loadingFixture.reset('alpha'));
    await until(loading, 'authoritative reset must not show an empty flash');
    await check(() => window.loadingFixture.release('alpha'));
    await until(opened, 'resync did not finish');
    assert.equal(await check(atBottom), true);

    win.webContents.debugger.attach('1.3');
    await win.webContents.debugger.sendCommand('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
    await check(() => { document.documentElement.classList.add('dark'); window.loadingFixture.select('beta'); });
    await until(loading, 'dark loader missing');
    assert.equal(await check(() => document.querySelector('.tr-loading').getAnimations({ subtree: true }).length), 0);
    await capture('loading-dark');
    win.setSize(580, 680);
    await pause(100);
    assert.equal(await check(() => {
      const loader = document.querySelector('.tr-loading');
      return loader.scrollWidth <= loader.clientWidth && loader.scrollHeight <= loader.clientHeight;
    }), true, 'narrow loader overflow');
    await capture('loading-narrow');
    await check(() => window.loadingFixture.release('beta'));
    await until(opened, 'reduced-motion reveal failed');
    assert.equal(await check(atBottom), true);
    assert.deepEqual(errors, [], 'renderer errors');
    console.log(JSON.stringify({ result: 'PASS', screenshots: temporary, checks: ['delayed snapshot', 'no empty or stale flash',
      'stable first visible frame', 'fold measurements', 'team readiness and continue', 'reconnect and resync', 'retry',
      'rapid switching', 'search landing', 'keyboard focus', 'light/dark/narrow', 'reduced motion'] }, null, 2));
  } catch (error) {
    failed = true; console.error(error);
    if (win) { await capture('failure'); console.error(await check(() => document.body.innerText.slice(-1800))); }
  } finally {
    clearInterval(pulse); clearTimeout(deadline); win?.destroy();
    await Promise.race([server?.close(), pause(1000)]);
    app.exit(failed ? 1 : 0);
  }
}
void run();
