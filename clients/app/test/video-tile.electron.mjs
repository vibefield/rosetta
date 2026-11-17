// A sent video in the production rail, in a hidden window: HistoryRoom over
// the room's demo ports, whose second transcript holds one captured and one
// uncaptured video. Three hosts, one page load each (`?host=`):
//
//   1. The failure ladder. The host answers a video by an address on this
//      fixture's own server, which holds its response until the test releases
//      it and then sends a hand-assembled, header-only MP4 (an `ftyp` box and
//      padding). No browser can decode that, so the run walks the whole
//      ladder: nothing asked before the press, the host's address in a
//      `<video>`, the bytes read once that address fails, and the "could not
//      play" face when the bytes cannot play either.
//   2. The bytes path, which is what rosetta's own app takes (its host offers
//      no `mediaAssetUrl`). The session's `readAsset` answers a real clip:
//      leaving while that read is pending aborts it and makes no URL; played,
//      the object URL decodes, seeks, and is revoked on leaving.
//   3. A host address that honours byte ranges (206): it plays and seeks
//      with no byte read through the session.
//
// No encoded video is committed. The real clip is recorded in the test window
// itself, from a painted canvas, as H.264 in MP4; a build that cannot record
// that fails the run by name.
//
// The page and its module are served by the plugin below rather than from
// files: the room's sources are imported as they are, through the app's own
// Vite config.
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { app, BrowserWindow } from 'electron';

const root = fileURLToPath(new URL('..', import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'rosetta-video-tile-'));
const headlessCi = process.platform === 'linux' && Boolean(process.env.CI);
if (headlessCi) app.disableHardwareAcceleration();
app.setPath('userData', path.join(temporary, 'profile'));
app.on('window-all-closed', () => {});
const pulse = setInterval(() => {}, 100);
const deadline = setTimeout(() => { console.error('Video tile check timed out'); app.exit(1); }, 90000);
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
let win, server;
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message, ...args) {
  const truthy = `(...a) => Boolean((${fn.toString()})(...a))`;
  for (let i = 0; i < 240; i++) {
    if (await win.webContents.executeJavaScript(`(${truthy})(...${JSON.stringify(args)})`)) return;
    await pause(25);
  }
  throw Error(message);
}
const click = selector => check(s => { const el = document.querySelector(s); if (!el) throw Error(`no ${s}`); el.click(); return true; }, selector);

// An `ftyp` box (24 bytes: isom, minor 512, compatible isom mp41) and a
// `free` box of padding. A header with no `moov` and no media.
const HEADER_ONLY_MP4 = Buffer.alloc(64);
HEADER_ONLY_MP4.writeUInt32BE(24, 0); HEADER_ONLY_MP4.write('ftypisom', 4, 'latin1'); HEADER_ONLY_MP4.writeUInt32BE(512, 12);
HEADER_ONLY_MP4.write('isommp41', 16, 'latin1'); HEADER_ONLY_MP4.writeUInt32BE(40, 24); HEADER_ONLY_MP4.write('free', 28, 'latin1');

// A short real clip, recorded in the page: about a second and a half of a
// painted 320×180 canvas. Returned as base64 for the fixture server to hold.
const CLIP_TYPE = 'video/mp4;codecs=avc1.42E01E';
const recordClip = async (type, ms) => {
  if (typeof MediaRecorder !== 'function' || !MediaRecorder.isTypeSupported(type)) {
    throw Error(`this build cannot record ${type}: no decodable clip to play, so nothing about playback was checked`);
  }
  const canvas = document.createElement('canvas');
  canvas.width = 320; canvas.height = 180;
  const context = canvas.getContext('2d');
  let frame = 0;
  const paint = () => {
    context.fillStyle = `hsl(${(frame += 7) % 360} 60% 50%)`; context.fillRect(0, 0, 320, 180);
    context.fillStyle = '#fff'; context.fillRect(frame % 300, 80, 20, 20);
  };
  paint();
  const painting = setInterval(paint, 33);
  const recorder = new MediaRecorder(canvas.captureStream(30), { mimeType: type, videoBitsPerSecond: 200000 });
  const chunks = [];
  recorder.ondataavailable = event => chunks.push(event.data);
  const stopped = new Promise((resolve, reject) => { recorder.onstop = resolve; recorder.onerror = event => reject(event.error ?? Error('the recorder failed')); });
  recorder.start();
  await new Promise(resolve => setTimeout(resolve, ms));
  recorder.stop();
  await stopped;
  clearInterval(painting);
  const bytes = new Uint8Array(await new Blob(chunks).arrayBuffer());
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
};

const PAGE = '/test/video-tile.html';
// The recorded clip as a session's client reads it: whole, off the asset route.
const CLIP = '/test/video-tile.clip.mp4';
const MODULE = '/test/video-tile.fixture.js';
const moduleId = path.join(root, 'test/video-tile.fixture.js');
const HTML = `<!doctype html>
<html lang="en" class="dark">
  <head><meta charset="utf-8"><title>Rosetta · A sent video</title>
    <style>html, body, #root { height: 100%; margin: 0; } body { background: #0e1116; }</style></head>
  <body><div id="root"></div><script type="module" src="${MODULE}"></script></body>
</html>`;
// The room over its demo ports. The host's doors are wrapped to record what
// the room asks of them. `?host=address`: `mediaAssetUrl` answers this
// server's own address, and a read answers the demo's header-only member.
// `?host=none`: the door is absent, as in rosetta's app, and a read answers
// the recorded clip, at once or when the test delivers it.
const FIXTURE = `
import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { HistoryRoom } from "../../room/src/HistoryRoom";
import { createDemoPorts, createRoomDemo } from "../../room/src/fixtures";
import "../renderer/src/styles/tokens.css";
import "../../room/src/styles/room.css";
import "../../room/src/styles/ingest.css";
import "../../room/src/styles/transcript.css";
import "../../room/src/styles/search.css";

const demo = createRoomDemo();
const base = createDemoPorts(demo);
const sent = demo.timelines.get("s8").find((row) => row.body.type === "tool_use").body.tool.attachments;
const host = new URLSearchParams(location.search).get("host");
const log = { images: [], media: [], reads: [], made: [], revoked: [], events: [] };
const signals = [];
const pending = [];
const clip = () => fetch(${JSON.stringify(CLIP)}).then((response) => response.arrayBuffer()).then((buffer) => new Uint8Array(buffer));
// Media events do not bubble; they are heard on their way down.
for (const name of ["playing", "seeked", "ended", "error"]) {
  document.addEventListener(name, (event) => { if (event.target instanceof HTMLVideoElement) log.events.push(name); }, true);
}
const make = URL.createObjectURL.bind(URL);
const revoke = URL.revokeObjectURL.bind(URL);
URL.createObjectURL = (blob) => { const url = make(blob); log.made.push({ url, type: blob.type, size: blob.size }); return url; };
URL.revokeObjectURL = (url) => { log.revoked.push(url); revoke(url); };
const ports = {
  catalog: base.catalog,
  sessions: {
    open(id) {
      const client = base.sessions.open(id);
      return { ...client, readAsset(asset, signal) {
        log.reads.push(asset.url);
        signals.push(signal);
        if (host !== "none") return client.readAsset(asset, signal);
        // Deliberately deaf to the signal: a held read's bytes arrive even
        // after the tile left, so the room itself must drop them.
        return fixture.holdReads ? new Promise((resolve) => { pending.push(() => resolve(clip())); }) : clip();
      } };
    },
    close(id) { base.sessions.close(id); },
  },
  assets: {
    imageAssetUrl(asset, variant) { log.images.push(asset.url); return base.assets.imageAssetUrl(asset, variant); },
    attachmentAssetUrl: (asset) => base.assets.attachmentAssetUrl(asset),
    ...(host === "none" ? {} : {
      mediaAssetUrl(asset) { log.media.push(asset.url); return new URL(asset.url, location.origin).toString(); },
    }),
  },
};
const fixture = {
  log, captured: sent[0].source.asset, uncaptured: sent[1], host, holdReads: false,
  hasMediaDoor: "mediaAssetUrl" in ports.assets,
  aborted: () => signals.map((signal) => signal.aborted),
  // Hand every held read its bytes, and wait for the room to have seen them.
  deliver: async () => {
    const count = pending.length;
    for (const go of pending.splice(0)) go();
    await clip();
    await new Promise((resolve) => setTimeout(resolve, 100));
    return count;
  },
};
window.videoFixture = fixture;
createRoot(document.getElementById("root")).render(
  createElement("div", { style: { height: "100%" } }, createElement(HistoryRoom, ports)));
`;

// What reached the asset route, as the server saw it, how it answered, and
// the responses it holds. `serving` is the route's behaviour for one case.
const requests = [];
const answers = [];
const held = [];
const release = () => held.splice(0).forEach(end => end());
const serving = { body: HEADER_ONLY_MP4, ranges: false, hold: true };
let clip = null;
const fixturePlugin = {
  name: 'video-tile-fixture',
  enforce: 'pre',
  resolveId(id) { return id === MODULE || id === moduleId ? moduleId : null; },
  load(id) { return id === moduleId ? FIXTURE : null; },
  configureServer(vite) {
    vite.middlewares.use((request, response, next) => {
      const url = new URL(request.url ?? '/', 'http://fixture');
      if (url.pathname === PAGE) {
        vite.transformIndexHtml(PAGE, HTML).then(html => { response.setHeader('content-type', 'text/html'); response.end(html); }, next);
        return;
      }
      if (url.pathname === CLIP) {
        response.setHeader('content-type', 'video/mp4');
        response.setHeader('cache-control', 'no-store');
        response.setHeader('content-length', String(clip.byteLength));
        response.end(clip);
        return;
      }
      if (!/^\/api\/assets\/[a-f0-9]{64}\.(mp4|mov)$/.test(url.pathname)) { next(); return; }
      requests.push(`${url.pathname}${url.search}`);
      // A host's own address for the member, in two shapes. The ladder's
      // answers whole (200, no ranges) because nothing ever plays from it:
      // its body is a header no browser can decode. That is NOT what a host
      // address should look like: Chromium plays a whole answer but cannot
      // seek it, and raises no error the room could fall back on. An address
      // a video plays from honours byte ranges (the 206 arm below). The
      // daemon answers a member whole, so rosetta's app offers no host
      // address at all and its videos take the bytes path.
      const { body, ranges } = serving;
      const answer = () => {
        response.setHeader('content-type', 'video/mp4');
        response.setHeader('cache-control', 'no-store');
        const asked = ranges ? /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? '') : null;
        if (ranges) response.setHeader('accept-ranges', 'bytes');
        if (asked === null) {
          answers.push(200);
          response.statusCode = 200;
          response.setHeader('content-length', String(body.byteLength));
          response.end(body);
          return;
        }
        const start = Number(asked[1]);
        const end = Math.min(asked[2] === '' ? body.byteLength - 1 : Number(asked[2]), body.byteLength - 1);
        if (start > end) {
          answers.push(416);
          response.statusCode = 416;
          response.setHeader('content-range', `bytes */${body.byteLength}`);
          response.end();
          return;
        }
        answers.push(206);
        response.statusCode = 206;
        response.setHeader('content-range', `bytes ${start}-${end}/${body.byteLength}`);
        response.setHeader('content-length', String(end - start + 1));
        response.end(body.subarray(start, end + 1));
      };
      if (serving.hold) held.push(answer); else answer();
    });
  },
};

async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import('vite');
    server = await createServer({ configFile: path.join(root, 'vite.config.ts'), root, logLevel: 'error', plugins: [fixturePlugin],
      server: { host: '127.0.0.1', port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
    win = new BrowserWindow({ width: 1440, height: 900, show: headlessCi,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    const errors = [];
    win.webContents.on('console-message', event => { if (event.level === 'error') errors.push(event.message); });
    const load = async host => {
      await win.loadURL(`${origin}${PAGE}?host=${host}`);
      await until(() => window.videoFixture, 'fixture ready');
      await until(() => document.querySelectorAll('.ah-project-list-item').length === 6, 'projects through the catalog port');
    };
    const openSent = async () => {
      await click('.ah-project-list-item[data-motion-key="p1"]');
      await until(() => document.querySelector('.ah-session-list-item[data-motion-key="s8"]'), 'the project\'s sessions');
      await click('.ah-session-list-item[data-motion-key="s8"]');
      await until(() => document.querySelectorAll('.tr-attachments figure.tr-att__video').length === 2, 'the run\'s two video tiles');
      // Let the transcript finish opening: a press in its first moments can
      // lose the focus the tile takes (the room is still settling its own).
      await pause(300);
    };
    const leave = async () => {
      await click('.ah-project-list-item[data-motion-key="p0"]');
      await until(() => !document.querySelector('.tr-attachments figure.tr-att__video'), 'the tiles leave with their transcript');
    };
    // A playing video, as the element itself reports it: paused, then sent
    // to a point past the middle of what it says it can seek.
    const played = async () => {
      await until(() => document.querySelector('.tr-attachments video'), 'the video element');
      await until(() => window.videoFixture.log.events.includes('playing'), 'the video plays');
      return check(async () => {
        const video = document.querySelector('.tr-attachments video');
        const ranges = Array.from({ length: video.seekable.length }, (_, i) => [video.seekable.start(i), video.seekable.end(i)]);
        const end = ranges.length === 0 ? 0 : ranges[ranges.length - 1][1];
        const target = Math.round(end * 60) / 100;
        video.pause();
        const seeked = new Promise(resolve => video.addEventListener('seeked', () => resolve(true), { once: true }));
        video.currentTime = target;
        const fired = await Promise.race([seeked, new Promise(resolve => setTimeout(() => resolve(false), 3000))]);
        return { src: video.getAttribute('src'), state: video.closest('figure').dataset.video, controls: video.controls,
          width: video.videoWidth, height: video.videoHeight, ranges, end, target, fired, landed: video.currentTime,
          error: video.error?.message ?? null, focused: document.activeElement === video };
      });
    };
    const assertPlayed = (result, where) => {
      assert.equal(result.error, null, `${where}: no element error`);
      assert.deepEqual([result.state, result.controls, result.focused], ['ready', true, true], `${where}: the ready face, focused`);
      assert.deepEqual([result.width, result.height], [320, 180], `${where}: a frame was decoded at the clip's own size`);
      assert.equal(result.ranges.length, 1, `${where}: one seekable range`);
      assert.ok(result.end > 0.5, `${where}: the seekable range ends above 0 (${result.end})`);
      assert.ok(result.target > 0.3, `${where}: the seek target is inside the clip (${result.target})`);
      assert.equal(result.fired, true, `${where}: the seek completes`);
      assert.ok(result.landed >= result.target && result.landed <= result.end, `${where}: a seek to ${result.target} lands at or past it (${result.landed})`);
    };
    const log = () => check(() => window.videoFixture.log);

    await load('address');
    // The real clip, recorded here before anything is drawn from it.
    clip = Buffer.from(await check(recordClip, CLIP_TYPE, 1500), 'base64');
    assert.ok(clip.byteLength > 2000, `a recorded clip, not a header (${clip.byteLength} bytes)`);
    assert.equal(clip.toString('latin1', 4, 8), 'ftyp', 'an MP4');
    await openSent();
    const { captured, uncaptured } = await check(() => ({ captured: window.videoFixture.captured, uncaptured: window.videoFixture.uncaptured }));
    const address = `${origin}${captured.url}`;

    // Before the press: a tile with a glyph, the name and the size. No
    // poster, no thumbnail, no address asked, no byte read, no request.
    const before = await check(() => {
      const [play, missing] = document.querySelectorAll('.tr-attachments figure.tr-att__video');
      const tile = play.querySelector('.tr-att__tile');
      const dashed = missing.querySelector('.tr-att__tile');
      return {
        rail: document.querySelectorAll('.tr-attachments').length,
        states: [play.dataset.video, missing.dataset.video],
        play: { tag: tile.tagName, label: tile.getAttribute('aria-label'), name: tile.querySelector('.tr-att__name').textContent,
          glyph: tile.querySelectorAll('svg').length, height: Math.round(tile.getBoundingClientRect().height),
          width: Math.round(tile.getBoundingClientRect().width) },
        missing: { tag: dashed.tagName, text: dashed.querySelector('span').textContent, facts: dashed.querySelector('.tr-att__facts').textContent,
          buttons: missing.querySelectorAll('button').length, height: Math.round(dashed.getBoundingClientRect().height),
          width: Math.round(dashed.getBoundingClientRect().width) },
        media: document.querySelectorAll('.tr-attachments video, .tr-attachments img').length,
        poster: document.querySelectorAll('[poster]').length,
      };
    });
    writeFileSync(path.join(temporary, 'before-play.png'), (await win.webContents.capturePage()).toPNG());
    assert.equal(before.rail, 1, 'one rail for the run');
    assert.deepEqual(before.states, ['idle', 'uncaptured']);
    assert.equal(before.play.tag, 'BUTTON');
    assert.match(before.play.label, /^Play video — room-walkthrough\.mp4 · 3\.40 MB$/);
    assert.deepEqual([before.play.name, before.play.glyph], ['room-walkthrough.mp4', 1]);
    assert.deepEqual([before.play.height, before.play.width], [144, 256], 'the rail\'s height, the video\'s 16:9');
    assert.deepEqual(before.missing, { tag: 'DIV', text: 'Video not captured', facts: `${uncaptured.name} · 11.0 MB`, buttons: 0, height: 144, width: 256 },
      'sent, not captured: the same shape, said in words, not a button, no Retry');
    assert.deepEqual([before.media, before.poster], [0, 0], 'no poster and no thumbnail');
    assert.deepEqual(requests, [], 'nothing asked of the asset route before the press');
    const idle = await log();
    assert.deepEqual([idle.media, idle.reads, idle.made, idle.images], [[], [], [], []], 'no address, no bytes, no image rendition for a video');

    // The press: an inline <video controls> at the host's address.
    await click('.tr-attachments figure.tr-att__video button');
    await until(() => document.querySelector('.tr-attachments video'), 'the video element');
    const playing = await check(() => {
      const video = document.querySelector('.tr-attachments video');
      return { src: video.getAttribute('src'), controls: video.controls, poster: video.getAttribute('poster'),
        state: video.closest('figure').dataset.video, focused: document.activeElement === video,
        buttons: video.closest('figure').querySelectorAll('button').length };
    });
    assert.deepEqual(playing, { src: address, controls: true, poster: null, state: 'ready', focused: true, buttons: 0 });
    for (let i = 0; i < 80 && requests.length === 0; i++) await pause(25);
    assert.deepEqual(requests, [captured.url], 'the element asks the member\'s plain address, once');
    const asked = await log();
    assert.deepEqual([asked.media, asked.reads, asked.made], [[captured.url], [], []], 'one address asked, no bytes read yet');

    // The rail's keys are the video's while it has focus: nothing is
    // prevented, focus stays, the rail does not move. On the tile around the
    // video the same key still walks the rail.
    const keys = await check(() => {
      const rail = document.querySelector('.tr-attachments .tr-att');
      const video = rail.querySelector('video');
      const left = rail.scrollLeft;
      const press = (target, key) => {
        const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
        target.dispatchEvent(event);
        return event.defaultPrevented;
      };
      video.focus({ preventScroll: true });
      const onVideo = ['ArrowRight', 'ArrowLeft', 'Home', 'End'].map(key => press(video, key));
      const stayed = document.activeElement === video && rail.scrollLeft === left;
      const tile = video.closest('.tr-att__tile');
      tile.focus({ preventScroll: true });
      const onTile = press(tile, 'ArrowRight');
      const moved = document.activeElement?.closest('figure')?.dataset.video;
      video.focus({ preventScroll: true });
      return { onVideo, stayed, onTile, moved };
    });
    assert.deepEqual(keys, { onVideo: [false, false, false, false], stayed: true, onTile: true, moved: 'uncaptured' });

    // The address answers bytes the browser cannot decode: the room reads the
    // member through the session and plays an object URL; that cannot play
    // either, so the tile says so, offers Retry, and the URL is revoked.
    release();
    await until(() => window.videoFixture.log.reads.length === 1, 'the bytes are read once the address fails');
    await until(() => document.querySelector('.tr-attachments figure[data-video="failed"]'), 'the could-not-play face');
    const failed = await check(() => {
      const figure = document.querySelector('.tr-attachments figure[data-video="failed"]');
      return { text: figure.querySelector('.tr-att__tile span').textContent, retry: figure.querySelectorAll('.tr-att__retry').length,
        named: figure.querySelector('.tr-att__retry').getAttribute('aria-label'),
        videos: document.querySelectorAll('.tr-attachments video').length,
        focused: document.activeElement === figure.querySelector('.tr-att__retry') };
    });
    assert.deepEqual(failed, { text: 'Could not play video', retry: 1, named: 'Retry video — room-walkthrough.mp4', videos: 0, focused: true },
      'focus follows the tile to Retry, which says what it retries');
    writeFileSync(path.join(temporary, 'could-not-play.png'), (await win.webContents.capturePage()).toPNG());
    const after = await log();
    assert.deepEqual(after.reads, [captured.url], 'read once, through the session');
    assert.equal(after.made.length, 1);
    assert.deepEqual([after.made[0].type, after.made[0].size], ['video/mp4', 64], 'an object URL over the member\'s bytes');
    assert.match(after.made[0].url, /^blob:/);
    assert.deepEqual(after.revoked, [after.made[0].url], 'and revoked when it could not play');
    assert.ok(requests.every(request => request === captured.url), 'only the member\'s plain address was ever requested');
    assert.deepEqual(after.images, [], 'a video is never handed to imageAssetUrl');

    // Retry asks the host again.
    const seen = requests.length;
    await click('.tr-attachments .tr-att__retry');
    await until(() => document.querySelector('.tr-attachments video'), 'the video again on Retry');
    assert.equal(await check(() => document.querySelector('.tr-attachments video').getAttribute('src')), address);
    assert.deepEqual((await log()).media, [captured.url, captured.url]);
    for (let i = 0; i < 80 && requests.length === seen; i++) await pause(25);
    assert.equal(requests.length, seen + 1, 'one more request for the retry');

    // Leaving the transcript takes the element with it.
    await leave();
    assert.equal(await check(() => document.querySelectorAll('.tr-attachments video').length), 0, 'the video leaves with its transcript');
    release();

    // ── A host with no video address, as rosetta's app is: the bytes path ──
    Object.assign(serving, { body: clip, ranges: false, hold: false });
    requests.length = 0; answers.length = 0;
    await load('none');
    assert.deepEqual(await check(() => [window.videoFixture.host, window.videoFixture.hasMediaDoor]), ['none', false], 'this host offers no mediaAssetUrl');

    // Leaving while the read is still pending: the read is aborted, and the
    // bytes that arrive afterwards make no URL.
    await check(() => { window.videoFixture.holdReads = true; });
    await openSent();
    await click('.tr-attachments figure.tr-att__video button');
    await until(() => document.querySelector('.tr-attachments figure[data-video="loading"]'), 'the loading face while the read is pending');
    const waiting = await check(() => {
      const button = document.querySelector('.tr-attachments figure[data-video="loading"] button');
      return { label: button.getAttribute('aria-label'), busy: button.getAttribute('aria-busy'), text: button.querySelector('.tr-att__facts').textContent,
        focused: document.activeElement === button, aborted: window.videoFixture.aborted(), videos: document.querySelectorAll('.tr-attachments video').length };
    });
    assert.deepEqual(waiting, { label: 'Loading video — room-walkthrough.mp4', busy: 'true', text: 'Loading video…', focused: true, aborted: [false], videos: 0 },
      'one read in flight, said aloud as loading');
    await leave();
    assert.deepEqual(await check(() => window.videoFixture.aborted()), [true], 'leaving aborts the read in flight');
    assert.equal(await check(() => window.videoFixture.deliver()), 1, 'the held read is handed its bytes after the tile left');
    const left = await log();
    assert.deepEqual([left.reads, left.made, left.revoked], [[captured.url], [], []], 'late bytes for a tile that left make no URL');
    assert.equal(await check(() => document.querySelectorAll('video').length), 0);

    // Played from the bytes: a real frame, a seekable range, a seek that lands.
    await check(() => { window.videoFixture.holdReads = false; });
    await openSent();
    await click('.tr-attachments figure.tr-att__video button');
    const fromBytes = await played();
    writeFileSync(path.join(temporary, 'playing-from-bytes.png'), (await win.webContents.capturePage()).toPNG());
    assertPlayed(fromBytes, 'bytes');
    const read = await log();
    assert.deepEqual(read.reads, [captured.url, captured.url], 'one read through the session for this press');
    assert.equal(read.made.length, 1, 'one object URL');
    assert.deepEqual([read.made[0].type, read.made[0].size], ['video/mp4', clip.byteLength], 'over the member\'s recorded bytes');
    assert.match(read.made[0].url, /^blob:/);
    assert.equal(fromBytes.src, read.made[0].url, 'the element plays the object URL');
    assert.deepEqual([read.media, read.revoked, read.images], [[], [], []], 'no host address asked, nothing revoked while it plays');
    assert.deepEqual(requests, [], 'the member\'s address is never loaded by an element on this host');
    const reachable = url => fetch(url).then(() => 'live', () => 'revoked');
    assert.equal(await check(reachable, read.made[0].url), 'live');

    // Leaving while that object URL is the element's src revokes it.
    await leave();
    const gone = await log();
    assert.deepEqual(gone.revoked, [read.made[0].url], 'the object URL is revoked when its tile leaves');
    assert.equal(gone.made.length, 1);
    assert.equal(await check(reachable, read.made[0].url), 'revoked', 'and the browser no longer resolves it');

    // ── A host address that honours byte ranges ──
    Object.assign(serving, { body: clip, ranges: true, hold: false });
    requests.length = 0; answers.length = 0;
    await load('address');
    await openSent();
    await click('.tr-attachments figure.tr-att__video button');
    const fromHost = await played();
    assertPlayed(fromHost, 'host address');
    assert.equal(fromHost.src, address, 'the element plays the host\'s address');
    assert.ok(requests.length > 0 && requests.every(request => request === captured.url), 'only the member\'s plain address was requested');
    assert.ok(answers.length > 0 && answers.every(status => status === 206), `every answer was a byte range (${answers.join(', ')})`);
    const ranged = await log();
    assert.deepEqual([ranged.media, ranged.reads, ranged.made, ranged.revoked, ranged.images], [[captured.url], [], [], [], []],
      'one address asked; no byte read through the session, no object URL');
    assert.ok(!ranged.events.includes('error'), 'the element raised no error');
    await leave();
    assert.deepEqual((await log()).revoked, [], 'the host\'s address is the host\'s to revoke');

    assert.deepEqual(errors.filter(message => !/Failed to load resource/.test(message)), []);
    console.log(`PASS: a sent video in the rail on the demo set — nothing asked before the press (${idle.media.length} addresses, ${idle.reads.length} reads, 0 requests), a <video controls> at the host's address, the rail's keys left to the video, the bytes read once the address failed, the could-not-play face with Retry and the object URL revoked, Retry asking the host again, an uncaptured video said in words; with no host address, a pending read aborted on leaving and its late bytes making no URL, a recorded ${clip.byteLength}-byte H.264 clip played from an object URL (${fromBytes.width}×${fromBytes.height}, seekable to ${fromBytes.end.toFixed(2)} s, a seek to ${fromBytes.target} landing at ${fromBytes.landed.toFixed(2)}) and that URL revoked on leaving; at a host address answering 206 the same clip played and seeked (seekable to ${fromHost.end.toFixed(2)} s, a seek to ${fromHost.target} landing at ${fromHost.landed.toFixed(2)}, ${answers.length} range answers, no bytes read). Screenshots: ${temporary}/before-play.png, ${temporary}/could-not-play.png, ${temporary}/playing-from-bytes.png`);
  } catch (error) {
    console.error(error);
    process.exitCode = 1;
  } finally {
    clearTimeout(deadline);
    release();
    await server?.close();
    app.exit(process.exitCode ?? 0);
  }
}
run();
