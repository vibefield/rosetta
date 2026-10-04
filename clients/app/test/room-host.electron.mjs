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
    await until(() => document.querySelectorAll('.ah-project-list-item').length === 6, 'projects through the catalog port');
    assert.equal(await check(() => document.querySelector('.ah-searchbar')), null, 'no strip without the search option');
    assert.equal(await check(() => document.querySelector('[data-ah-sheet]')), null, 'no sheet without the search option');
    // RO-14: and the empty transcript names no key the host does not bind.
    assert.equal(await check(() => document.querySelector('.tr-scroll .ah-empty p')?.textContent), 'Pick a session.', 'no search, no key named');
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => document.querySelectorAll('.ah-session-list-item').length === 8, 'the project\'s sessions');
    await click('.ah-session-list-item[data-motion-key="s0"]');
    await until(() => document.querySelector('[data-timeline-id="s0-t1"]'), 'the transcript through the session port');
    assert.deepEqual(await check(() => window.roomFixture.log.opened), ['s0'], 'one open for the session being read');
    assert.deepEqual(await check(() => window.roomFixture.log.selections), [null,
      { sessionId: 's0', nativeSessionId: 'native-claude-0', folder: '/Users/demo/Projects/vibe-field' }], 'the host hears the selection');
    await pause(300);
    writeFileSync(path.join(temporary, 'plain.png'), (await win.webContents.capturePage()).toPNG());

    // Every image the room draws is addressed by the host's asset source.
    const tiles = await check(() => [...document.querySelectorAll('.tr-attachments img')].map(img => img.getAttribute('src').slice(0, 19)));
    assert.deepEqual(tiles, ['data:image/svg+xml,', 'data:image/svg+xml,'], 'both tiles load what the source answered');
    const { imageA, imageB } = await check(() => ({ imageA: window.roomFixture.imageA, imageB: window.roomFixture.imageB }));
    assert.deepEqual(await check(() => [...new Set(window.roomFixture.log.images)].sort()), [`${imageA}:tile`, `${imageB}:tile`].sort(), 'one rendition asked per tile');
    assert.equal(await check((a, b) => window.roomFixture.log.attachments.includes(a) && window.roomFixture.log.attachments.includes(b), imageA, imageB), true, 'attachments named by the source');
    await click('.tr-attachments .tr-att__tile');
    await until(() => document.querySelector('.ah-image img'), 'the viewer\'s image');
    assert.equal(await check(() => document.querySelector('.ah-image img').getAttribute('src').startsWith('data:image/svg+xml,')), true);
    await until(() => document.querySelectorAll('.ah-image-gallery__thumb img').length === 2, 'the gallery\'s thumbnails');
    assert.deepEqual(await check(() => [...new Set(window.roomFixture.log.images)].sort()), [`${imageA}:preview`, `${imageA}:rail`, `${imageA}:tile`, `${imageB}:rail`, `${imageB}:tile`].sort(), 'the viewer and its rail ask the source too');
    assert.equal(await check(() => [...document.querySelectorAll('img')].every(img => /^(data|blob):/.test(img.getAttribute('src')))), true, 'no image the source did not answer');
    // An open dialog marks the room's root, and the stage recedes behind it.
    assert.equal(await check(() => document.querySelector('.ah-frame').hasAttribute('data-ah-overlay')), true, 'the root is marked');
    await until(() => getComputedStyle(document.querySelector('.ah-room')).transform === 'matrix(0.98, 0, 0, 0.98, 0, 0)', 'the stage recedes');
    assert.equal(await check(() => document.querySelector('.ah-fileview').classList.contains('ah-layer') && document.querySelector('.ah-fileview').parentElement === document.body), true, 'a paper room portals to the body');
    await check(() => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); document.activeElement?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); return true; });
    await until(() => !document.querySelector('.ah-image img'), 'the viewer closes');
    await until(() => !document.querySelector('.ah-frame').hasAttribute('data-ah-overlay'), 'the mark leaves with the dialog');
    // The room's own tokens travel with it: no page stylesheet defines the band.
    assert.equal(await check(() => getComputedStyle(document.querySelector('.ah-frame')).getPropertyValue('--tint-l').trim()), '0.74', 'the dark tint band');
    assert.equal(await check(() => getComputedStyle(document.documentElement).getPropertyValue('--tint-l').trim()), '', 'and nothing set on the page');
    assert.equal(await check(() => getComputedStyle(document.querySelector('.ah-room')).backgroundColor), 'rgba(28, 28, 30, 0.96)', 'paper: the room paints its plate');

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

    // A host that paints its own surface: no ground, no plate, frost for the
    // fades, and the room's layers land in the host's container.
    await mount('glass');
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => document.querySelector('.ah-session-list-item[data-motion-key="s0"]'), 'sessions on glass');
    await click('.ah-session-list-item[data-motion-key="s0"]');
    await until(() => document.querySelector('[data-timeline-id="s0-t1"]'), 'transcript on glass');
    const glass = await check(() => {
      const css = (selector, pseudo) => getComputedStyle(document.querySelector(selector), pseudo);
      return {
        surface: document.querySelector('.ah-frame').dataset.ahSurface,
        ground: css('.ah-frame').backgroundImage, plate: css('.ah-room').backgroundColor, plateFilter: css('.ah-room').backdropFilter,
        frost: css('.tr-frost', '::before').backdropFilter, frostPaint: css('.tr-frost', '::before').backgroundImage,
        frostMask: css('.tr-frost', '::before').webkitMaskImage.startsWith('linear-gradient'),
        day: css('.ah-day', '::before').backdropFilter, dayPaint: css('.ah-day').backgroundImage,
      };
    });
    assert.deepEqual(glass, { surface: 'host', ground: 'none', plate: 'rgba(0, 0, 0, 0)', plateFilter: 'none',
      frost: 'blur(14px)', frostPaint: 'none', frostMask: true, day: 'blur(14px)', dayPaint: 'none' }, 'the host paints beneath');
    await check(() => { const s = document.querySelector('.tr-scroll'); s.scrollTop = 60; s.dispatchEvent(new Event('scroll')); return true; });
    await pause(300);
    writeFileSync(path.join(temporary, 'glass.png'), (await win.webContents.capturePage()).toPNG());
    await check(() => { const s = document.querySelector('.tr-scroll'); s.scrollTop = 0; s.dispatchEvent(new Event('scroll')); return true; });
    await click('.tr-attachments .tr-att__tile');
    await until(() => document.querySelector('#host-layer .ah-fileview'), 'the viewer in the host\'s container');
    assert.equal(await check(() => document.querySelector('body > .ah-fileview, body > div > .ah-fileview:not(#host-layer *)')), null, 'nothing portalled to the body');
    await check(() => { document.activeElement?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); return true; });
    await until(() => !document.querySelector('#host-layer .ah-fileview'), 'the viewer closes in the host\'s container');

    // openAt before the index has delivered: the room waits, then opens the
    // session's thread in its project and lands on the message.
    await mount('late');
    await check(() => { window.roomFixture.openAt({ sessionId: 's8', timelineId: 's8-t2' }); return true; });
    await until(() => document.querySelector('.ah-project-list-item[data-motion-key="p1"].is-selected'), 'the session\'s project');
    await until(() => document.querySelector('.ah-session-list-item[data-motion-key="s8"].is-selected'), 'the session itself');
    await until(() => document.querySelector('[data-hit] [data-timeline-id="s8-t2"], [data-hit][data-timeline-id="s8-t2"], [data-timeline-id="s8-t2"] [data-hit]'), 'landed on the message');
    assert.deepEqual(await check(() => window.roomFixture.log.selections.at(-1)), { sessionId: 's8', nativeSessionId: 'native-claude-8', folder: '/Users/demo/Projects/rosetta' });
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => window.roomFixture.log.selections.at(-1) === null, 'a new project clears the selection the host hears');
    await check(() => { window.roomFixture.openAt({ sessionId: 'nope' }); return true; });
    await until(() => document.querySelector('.ah-receipt')?.textContent === 'That session is not in this index', 'an unknown session is said so');

    // The host sizes the room: its edge inset, and where lists and floating
    // controls end.
    await mount('sized');
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => document.querySelector('.ah-session-list-item[data-motion-key="s0"]'), 'sessions when sized');
    await click('.ah-session-list-item[data-motion-key="s0"]');
    await until(() => document.querySelector('.ah-col--measure .ah-newpill'), 'the floating control');
    assert.deepEqual(await check(() => {
      const room = getComputedStyle(document.querySelector('.ah-room'));
      return [room.top, room.right, room.bottom, room.left, getComputedStyle(document.querySelector('.tr-scroll')).paddingBottom,
        getComputedStyle(document.querySelector('.ah-col--measure .ah-newpill')).bottom];
    }), ['12px', '8px', '8px', '8px', '48px', '40px']);

    // The guest closes by the room's own width, not the window's: a host that
    // seats the room in 1200px of a 1440px window gets no guest until asked.
    await mount('narrow-host');
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => document.querySelector('[aria-label="Toggle the project worktree"]'), 'an engine offers the worktree');
    assert.equal(await check(() => window.innerWidth >= 1360 && document.querySelector('.ah-room').clientWidth === 1200), true);
    assert.equal(await check(() => document.querySelector('.ah-cols').dataset.rail), 'off', 'the guest yields in a narrow room');
    await mount('wide-host');
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => document.querySelector('.ah-cols')?.dataset.rail === 'on', 'the guest opens in a wide room');

    // Every name the room keeps goes under the host's prefix, in the host's
    // store: dealt tints, the fold, the site-icon setting, recent searches.
    const before = await check(() => Object.keys(localStorage).sort());
    await mount('prefixed');
    await until(() => document.querySelectorAll('.ah-project-list-item').length === 6, 'projects, tints dealt');
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => document.querySelector('.ah-session-list-item[data-motion-key="s0"]'), 'sessions under a prefix');
    await click('.ah-session-list-item[data-motion-key="s0"]');
    await until(() => document.querySelector('.ah-col--measure .ah-iconbtn[aria-pressed]'), 'the activity act');
    await click('.ah-col--measure .ah-iconbtn[aria-pressed]');
    await until(() => document.querySelector('.tr-line[data-timeline-id="s0-t5"]'), 'the web line, every run open');
    await click('.tr-line[data-timeline-id="s0-t5"]');
    await until(() => document.querySelector('.tr-site-icons input'), 'the site-icon setting');
    await click('.tr-site-icons input');
    await click('.ah-searchbar .ah-launcher__open');
    await until(() => document.querySelector('.ah-sheet__field input'), 'the sheet');
    await check(() => {
      const input = document.querySelector('.ah-sheet__field input');
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, 'package');
      input.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    });
    await until(() => document.querySelector('.ah-sheet__row'), 'a hit');
    await click('.ah-sheet__row');
    await until(() => document.querySelector('.ah-session-list-item[data-motion-key="s8"].is-selected') && !document.querySelector('.ah-sheet'), 'the hit opens its session');
    assert.deepEqual(await check(() => window.roomFixture.stored()),
      ['vf.history.search.recent', 'vf.history.siteIcons.v1', 'vf.history.tintSeq.v1', 'vf.history.transcript.fold.v2'], 'four names, one prefix');
    assert.deepEqual(await check(() => Object.keys(localStorage).sort()), before, 'nothing in the page\'s own storage');

    // A source that fetches before it can address: tiles wait, then load its
    // object URLs; a refusal is the "could not load" face, and Retry asks again.
    await mount('fetching');
    await click('.ah-project-list-item[data-motion-key="p0"]');
    await until(() => document.querySelector('.ah-session-list-item[data-motion-key="s0"]'), 'sessions while fetching');
    await click('.ah-session-list-item[data-motion-key="s0"]');
    await until(() => document.querySelectorAll('.tr-attachments figure').length === 2, 'two tiles');
    await until(() => document.querySelector('.tr-attachments img')?.getAttribute('src')?.startsWith('blob:'), 'an object URL once fetched');
    await until(() => document.querySelector('.tr-att__retry'), 'the refused image\'s face');
    assert.equal(await check(() => document.querySelector('.tr-att__tile--missing span').textContent), 'Could not load image');
    await click('.tr-att__retry');
    await until(() => document.querySelectorAll('.tr-attachments img').length === 2, 'the retry asks the source again and loads');
    assert.equal(await check((b) => window.roomFixture.log.images.filter(name => name === `${b}:tile`).length, imageB), 2, 'asked once, then once more on retry');

    // The search option brings the room's strip, with the host's accessory in it.
    await mount('search');
    await until(() => document.querySelector('.ah-searchbar .ah-launcher'), 'the launcher in the strip');
    assert.equal(await check(() => document.querySelector('.ah-searchbar [data-accessory]')?.textContent), 'host');
    // RO-14: with no word about its key, the room prints its own label for the
    // key rosetta's app binds, and the empty transcript names that same key.
    await until(() => document.querySelector('.tr-scroll .ah-empty p'), 'the empty transcript with search');
    const ownKey = await check(() => document.querySelector('.ah-launcher .ah-kbd')?.textContent);
    assert.ok(ownKey === '⌘K' || ownKey === 'Ctrl K', `the room's own label: ${ownKey}`);
    assert.equal(await check(() => document.querySelector('.tr-scroll .ah-empty p').textContent),
      `Pick a session — or press ${ownKey} to search, which is the front door.`, 'the sentence names the printed key');
    // A host that binds no key: no legend on the launcher, none in the sentence.
    await mount('search-unbound');
    await until(() => document.querySelector('.ah-searchbar .ah-launcher') && document.querySelector('.tr-scroll .ah-empty p'), 'the strip, unbound');
    assert.equal(await check(() => document.querySelector('.ah-launcher .ah-kbd')), null, 'no legend for a key nobody binds');
    assert.equal(await check(() => document.querySelector('.tr-scroll .ah-empty p').textContent), 'Pick a session.');
    // A host that binds its own key: the room prints the host's label, as given.
    await mount('search-host-key');
    await until(() => document.querySelector('.ah-launcher .ah-kbd') && document.querySelector('.tr-scroll .ah-empty p'), 'the strip, the host\'s key');
    assert.equal(await check(() => document.querySelector('.ah-launcher .ah-kbd').textContent), '⌥Space');
    assert.equal(await check(() => document.querySelector('.tr-scroll .ah-empty p').textContent),
      'Pick a session — or press ⌥Space to search, which is the front door.');
    // A host's own sentence stands in for the room's.
    await mount('worded');
    await until(() => document.querySelector('.tr-scroll .ah-empty [data-host-prompt]'), 'the host\'s sentence');
    assert.equal(await check(() => document.querySelector('.tr-scroll .ah-empty p').textContent), 'Find one from the Peek.');

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
    console.log(`PASS: the room behind its ports on the demo set — catalog and one session open, every image through the asset source, the overlay mark, its own tokens, no host's honest faces, a host without an engine, a host's surface and container, openAt and the selection, the host's inset and feet, the guest by the room's width, the storage prefix, a fetching and a refusing source, the search option, the search key only as the host binds it (RO-14), unavailable words, consent. Screenshot: ${temporary}/room.png`);
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
