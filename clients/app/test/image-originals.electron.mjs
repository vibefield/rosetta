// A mixed projection in the production transcript and viewer, in a hidden
// window: a legacy WebP bundle keeps its own members, recorded originals are
// drawn only through `?image=<variant>&v=1` renditions — Fit and 1:1 included —
// and a tile whose rendition is still coming is busy until it draws.
import assert from 'node:assert/strict';
import { mkdtempSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { app, BrowserWindow } from 'electron';

const root = fileURLToPath(new URL('..', import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'rosetta-image-originals-'));
app.setPath('userData', path.join(temporary, 'profile'));
app.on('window-all-closed', () => {});
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error('Recorded-originals check timed out'); process.exit(1); }, 60000);
let win, server, failed = false;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message, ...args) {
  for (let i = 0; i < 200; i++) { if (await check(fn, ...args)) return; await pause(25); }
  throw new Error(message);
}
const reads = () => check(async () => (await fetch('/test/image-reads')).json());
const tile = index => check(index => {
  const button = document.querySelectorAll('[data-timeline-id="mixed"] figure')[index]?.querySelector('.tr-att__tile');
  const img = button?.querySelector('img');
  return button ? { className: button.className, busy: button.getAttribute('aria-busy'), src: img?.getAttribute('src') ?? null,
    width: img?.naturalWidth ?? 0 } : null;
}, index);

async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import('vite');
    server = await createServer({ configFile: path.join(root, 'test/image-originals.vite.config.ts'), root,
      logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 940, height: 900, show: false, webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    const errors = [];
    win.webContents.on('console-message', event => { if (event.level === 'error') errors.push(event.message); });
    const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
    await win.loadURL(`${origin}/test/image-originals.html`);
    await until(() => document.querySelectorAll('[data-timeline-id="mixed"] figure').length === 3, 'Mixed strip did not mount');

    // The legacy bundle and the fast original draw; the slow original is busy.
    await until(index => document.querySelectorAll('[data-timeline-id="mixed"] figure')[index]?.querySelector('img')?.naturalWidth > 0,
      'Legacy tile did not draw', 0);
    await until(index => document.querySelectorAll('[data-timeline-id="mixed"] figure')[index]?.querySelector('img')?.naturalWidth > 0,
      'Original tile did not draw', 1);
    const [legacy, recorded, slow] = [await tile(0), await tile(1), await tile(2)];
    assert.equal(legacy.className, 'tr-att__tile');
    assert.equal(legacy.busy, 'false');
    assert.doesNotMatch(legacy.src, /\?/, 'a legacy bundle keeps its own member address');
    assert.equal(recorded.className, 'tr-att__tile');
    assert.equal(recorded.busy, 'false');
    assert.match(recorded.src, new RegExp(`^${origin}/api/assets/1{64}\\.png\\?image=tile2?x?&v=1$`));
    assert.equal(slow.className, 'tr-att__tile tr-att__tile--loading');
    assert.equal(slow.busy, 'true');
    assert.equal(slow.width, 0);
    await check(async () => (await fetch('/test/release-slow')).text());
    await until(index => document.querySelectorAll('[data-timeline-id="mixed"] figure')[index]?.querySelector('img')?.naturalWidth > 0,
      'Released tile did not draw', 2);
    await until(index => document.querySelectorAll('[data-timeline-id="mixed"] figure')[index]?.querySelector('.tr-att__tile')?.getAttribute('aria-busy') === 'false',
      'Released tile stayed busy', 2);
    assert.equal((await tile(2)).className, 'tr-att__tile');

    // Fit and 1:1 of a recorded original are renditions, never its bytes.
    await check(() => document.querySelectorAll('[data-timeline-id="mixed"] figure')[1].querySelector('button.tr-att__tile').click());
    await until(() => document.querySelector('.ah-image__frame img')?.naturalWidth === 320, 'Fit view did not draw the preview rendition');
    assert.match(await check(() => document.querySelector('.ah-image__frame img').getAttribute('src')), /1{64}\.png\?image=preview&v=1$/);
    assert.equal(await check(() => document.querySelector('.ah-image').getAttribute('aria-busy')), 'false');
    await check(() => [...document.querySelectorAll('.ah-fileview__acts button')].find(button => button.textContent === '1:1').click());
    await until(() => document.querySelector('.ah-image')?.dataset.mode === 'actual'
      && /image=full&v=1$/.test(document.querySelector('.ah-image__frame img')?.getAttribute('src') ?? '')
      && document.querySelector('.ah-image__frame img').naturalWidth === 320, '1:1 did not draw the full rendition');
    await until(() => [...document.querySelectorAll('.ah-image-gallery__thumb img')].length === 3
      && [...document.querySelectorAll('.ah-image-gallery__thumb img')].every(img => img.naturalWidth > 0), 'Gallery rail did not draw');

    // The legacy bundle's 1:1 is its own full member, exactly as before.
    await check(() => document.querySelectorAll('.ah-image-gallery__thumb')[0].click());
    await until(() => document.querySelector('.ah-image__frame img')?.naturalWidth === 960, 'Legacy 1:1 did not draw');
    assert.match(await check(() => document.querySelector('.ah-image__frame img').getAttribute('src')),
      new RegExp(`/api/assets/${'196b946f8d285113fe65d920d490bb82cbed79d9e334e884ec395e128b6574b0'}\\.webp$`));

    const served = await reads();
    assert.equal(served['png:plain'], undefined, 'no display path asks for a recorded original');
    assert.equal(served['slow:plain'], undefined, 'no display path asks for a recorded original');
    for (const name of ['png:preview', 'png:full', 'png:rail', 'slow:rail']) assert.ok(served[name] > 0, name);
    assert.ok(Object.keys(served).every(name => !name.startsWith('legacy:') || name === 'legacy:plain'), JSON.stringify(served));
    assert.deepEqual(await check(() => window.imageOriginals.assetReads), [], 'the renderer buffers no image bytes');
    assert.deepEqual(errors, []);
    console.log(JSON.stringify({ result: 'passed', checks: ['legacy bundle members unchanged', 'original tile rendition', 'busy tile until drawn',
      'Fit and 1:1 renditions', 'gallery rail renditions', 'no original byte read'], served }));
  } catch (error) { failed = true; console.error(error); console.error('Artifacts:', temporary); }
  finally { clearTimeout(deadline); clearInterval(pulse); win?.destroy(); await Promise.race([server?.close(), pause(1000)]); app.exit(failed ? 1 : 0); }
}
void run();
