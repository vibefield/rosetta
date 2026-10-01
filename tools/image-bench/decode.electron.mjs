// Chromium fetch+decode comparison. Run after campaign.py with the same artifacts.
// Electron tools/image-bench/decode.electron.mjs /absolute/campaign/output
import { app, BrowserWindow } from 'electron';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
const root = path.resolve(process.argv[2]);
app.setPath('userData', fs.mkdtempSync(path.join(os.tmpdir(), 'rosetta-image-decode-')));
app.on('window-all-closed', () => {});
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error('Image decode benchmark timed out'); app.exit(1); }, 60000);
const files = new Map();
const rows = {};
for (const side of ['before', 'after']) {
  rows[side] = JSON.parse(fs.readFileSync(path.join(root, side, '0', 'images.json'))).map((image, index) => {
    const original = image.source.asset;
    const asset = side === 'before' ? original : original.image_variants.tile;
    const url = `/${side}/${index}`;
    files.set(url, { path: path.join(root, side, '0', 'db.sqlite.assets', asset.url.split('/').pop()), mime: asset.media_type });
    return { url, bytes: asset.byte_len };
  });
}
const server = http.createServer((req, res) => {
  const key = new URL(req.url, 'http://localhost').pathname;
  if (key === '/') { res.setHeader('content-type', 'text/html'); res.end('<!doctype html><title>Image decode fixture</title>'); return; }
  const entry = files.get(key);
  if (!entry) { res.statusCode = 404; res.end(); return; }
  res.setHeader('content-type', entry.mime); res.setHeader('cache-control', 'no-store');
  res.end(fs.readFileSync(entry.path));
});
let win;
async function run() {
try {
  await app.whenReady(); console.log('Electron ready');
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  win = new BrowserWindow({ show: true, width: 900, height: 600, webPreferences: { backgroundThrottling: false, contextIsolation: true } });
  await win.loadURL(`http://127.0.0.1:${server.address().port}/`);
  console.log('Decode fixture loaded');
  const result = { electron: process.versions.electron, chrome: process.versions.chrome,
    description: '20 row images, four concurrent loads; before fetch+Blob original PNG, after direct WebP tile URL; cache disabled; includes local HTTP and decode',
    before: [], after: [] };
  for (let repeat = 0; repeat < 5; repeat++) {
    for (const side of repeat % 2 ? ['after', 'before'] : ['before', 'after']) {
      const run = await win.webContents.executeJavaScript(`(async (rows, side) => {
        const timings = [], resources = []; let cursor = 0, pixels = 0;
        const start = performance.now();
        await Promise.all(Array.from({length: 4}, async () => {
          while (cursor < rows.length) {
            const row = rows[cursor++], t = performance.now();
            const img = new Image(); img.decoding = 'async';
            if (side === 'before') {
              const bytes = await (await fetch(row.url)).arrayBuffer();
              const url = URL.createObjectURL(new Blob([bytes], {type: 'image/png'}));
              resources.push(url); img.src = url;
            } else img.src = row.url;
            await img.decode(); pixels += img.naturalWidth * img.naturalHeight;
            timings.push(performance.now() - t); img.removeAttribute('src');
          }
        }));
        resources.forEach(url => URL.revokeObjectURL(url));
        timings.sort((a,b) => a-b);
        return { totalMs: performance.now()-start, p50Ms: timings[Math.floor(timings.length*.5)],
          p95Ms: timings[Math.floor(timings.length*.95)], decodedPixels: pixels, bytes: rows.reduce((n,r) => n+r.bytes, 0) };
      })(${JSON.stringify(rows[side])}, ${JSON.stringify(side)})`);
      result[side].push(run); console.log(side, repeat, run.totalMs);
    }
  }
  fs.writeFileSync(path.join(root, 'decode.json'), JSON.stringify(result, null, 2) + '\n');
  console.log(JSON.stringify(result));
  win.destroy(); server.close(); clearInterval(pulse); clearTimeout(deadline); app.exit(0);
} catch (error) { console.error(error); server.close(); app.exit(1); }

}
void run();
