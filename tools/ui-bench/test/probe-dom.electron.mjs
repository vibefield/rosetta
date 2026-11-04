// probe.js's in-page detectors in a real Chromium page, on fixtures only: a hidden
// Electron window on a private scheme, a mock WebSocket and fetch installed before the
// probe (so it wraps them as it wraps the app's), synthetic /ws/meta frames and
// /api/status bodies, and hand-built DOM in the M2 contract's shape. Trusted input
// goes through CDP Input.*, as in a run. It plays one breach of each kind the probe
// must catch — I1 (1) ready before receipt, (2) revert, (3) data-driven selection,
// (4) flash of empty, (7) wrong pending course; an H5 pairing; an H7 unrequested move
// and an aim miss — and prints the probe's export as one JSON line for
// probe-dom.test.mjs to judge. Run: electron tools/ui-bench/test/probe-dom.electron.mjs
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { BrowserWindow, app, protocol } from 'electron';

const here = path.dirname(fileURLToPath(import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'ui-bench-probe-dom-'));
app.setPath('userData', path.join(temporary, 'profile'));
protocol.registerSchemesAsPrivileged([{ scheme: 'uibt', privileges: { standard: true, secure: true, supportFetchAPI: true } }]);
const deadline = setTimeout(() => { console.error('probe DOM test timed out'); app.exit(1); }, 60_000);
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// Installed before the probe: the probe's wrappers extend/wrap these, as they do the
// app's natives. The binding is a plain function collecting what the probe emits.
const MOCKS = `
window.__emitted = [];
window.__uiBenchEmit = (msg) => { window.__emitted.push(msg); };
window.__sockets = [];
class MockWebSocket extends EventTarget {
  constructor(url) { super(); this.url = String(url); this.readyState = 1; window.__sockets.push(this); }
  send() {} close() {}
}
window.WebSocket = MockWebSocket;
window.__status = '{}';
window.fetch = async (input) => new Response(window.__status, { status: 200, headers: { 'content-type': 'application/json' } });
window.__uiBenchConfig = { giantNative: null };
`;

const T = 1_790_000_000_000;
const HOUR = 3_600_000;
const row = (id, over = {}) => ({ id, native_session_id: `native-${id}`, runtime_id: 'runtime-x', project_id: 'proj-one', cwd: '/private/cwd', parent_session_id: null,
  delegation: null, root_session_id: id, role: 'main', title: `title of ${id}`, summary: null, started_at: 0, updated_at: T, activity: null, stream_state: 'pending', ...over });
const SESSIONS = [
  row('sess-alpha', { updated_at: T }),
  row('sess-bravo', { updated_at: T - HOUR }),
  row('sess-delta', { updated_at: T - 2 * HOUR }),
  row('sess-echo', { updated_at: T - 3 * HOUR }),
  row('sess-golf', { project_id: 'proj-two', updated_at: T - 50 * HOUR }),
];
const status = (phase) => JSON.stringify({ canonical_ready: false, search: { readiness: 'building' },
  daemon: { runtimes: [{ kind: 'claude-code' }, { kind: 'codex' }], ingest: { phase, metadata_ready_at: T, runtimes: [], projects: [{ sessions_done: 0, sessions_total: 5 }], run: { state: 'running' } } } });

async function run() {
  let failed = false;
  let win;
  try {
    await app.whenReady();
    protocol.handle('uibt', () => new Response('<!doctype html><html><head><meta charset="utf-8"></head><body style="margin:0"><div id="root"></div></body></html>',
      { headers: { 'content-type': 'text/html' } }));
    win = new BrowserWindow({ width: 1000, height: 800, show: false, webPreferences: { contextIsolation: true, nodeIntegration: false, backgroundThrottling: false } });
    await win.loadURL('about:blank');
    const dbg = win.webContents.debugger;
    dbg.attach('1.3');
    const probe = readFileSync(path.join(here, '..', 'probe.js'), 'utf8');
    const worker = readFileSync(path.join(here, '..', 'probe-worker.js'), 'utf8');
    await dbg.sendCommand('Page.enable');
    await dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', { source: `${MOCKS}window.__uiBenchWorkerSrc=${JSON.stringify(worker)};\n${probe}` });
    await win.loadURL('uibt://t/index.html');
    const js = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
    const settle = () => pause(120); // a few animation frames: the probe's pass runs on the next one
    const press = async ({ x, y }) => {
      for (const type of ['mousePressed', 'mouseReleased']) {
        await dbg.sendCommand('Input.dispatchMouseEvent', { type, x, y, button: 'left', buttons: type === 'mousePressed' ? 1 : 0, clickCount: 1 });
      }
    };
    const centre = (selector) => js((sel) => { const r = document.querySelector(sel).getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + Math.min(r.height / 2, 10) }; }, selector);
    const feed = (frame) => js((f) => { const s = window.__sockets.find((x) => x.url.endsWith('/ws/meta')); s.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(f) })); }, frame);
    const poll = (phase) => js(async (body) => { window.__status = body; await fetch('/api/status'); }, status(phase));
    const sessionRow = (id, standing, state) => `<div class="ah-session-list-item" data-motion-key="${id}" data-standing="${standing}" data-stream-state="${state}" style="height:40px">${id}</div>`;
    const drawSessions = (rows) => js((html) => { document.querySelector('.ah-session-list').innerHTML = html; }, rows.join(''));

    // Consent: a trusted press on Begin (the probe's import clock starts there).
    await js(() => { document.getElementById('root').innerHTML = '<button data-action="begin-ingest" style="width:200px;height:40px">Begin</button>'; });
    await settle();
    await press(await centre('button[data-action="begin-ingest"]'));
    await settle();
    // The meta socket, its first snapshot, and the first post-consent status (freeze).
    await js(() => { new WebSocket('ws://127.0.0.1:9/ws/meta'); });
    await feed({ frame: 'hello', protocol_version: 29, stamp: null });
    await feed({ frame: 'meta_snapshot', runtimes: [{ id: 'runtime-x', kind: 'claude-code' }], projects: [{ id: 'proj-one' }, { id: 'proj-two' }], sessions: SESSIONS });
    await poll('backfilling');
    await settle();
    // The columns: two project cards in a scroller, the selected project's sessions.
    await js(() => {
      document.getElementById('root').innerHTML = '<div class="ah-cols" style="display:flex">'
        + '<div class="ah-col ah-scroll" style="height:600px;overflow:auto;width:300px"><div class="ah-project-list" id="plist">'
        + '<div class="ah-project-list-item" data-motion-key="proj-one" style="height:60px">one</div>'
        + '<div class="ah-project-list-item" data-motion-key="proj-two" style="height:60px">two</div></div></div>'
        + '<div class="ah-col ah-scroll" style="height:600px;overflow:auto;width:300px"><div class="ah-session-list"></div></div>'
        + '<div class="ah-col" id="stagecol" style="width:300px"></div></div>';
    });
    await drawSessions([sessionRow('sess-alpha', 'not-imported', 'pending'), sessionRow('sess-bravo', 'not-imported', 'pending'),
      sessionRow('sess-delta', 'not-imported', 'pending'), sessionRow('sess-echo', 'not-imported', 'pending')]);
    await settle();

    // H5 pairing: alpha drawn not imported, its flip frame arrives, then drawn ready.
    await feed({ frame: 'mutation', stamp: null, mutation: { type: 'session', ...SESSIONS[0], stream_state: 'indexed' } });
    await feed({ frame: 'mutation', stamp: null, mutation: { type: 'session', ...SESSIONS[2], stream_state: 'indexed' } });
    await settle();
    // I1(1): bravo drawn ready although no frame ever made it so.
    await drawSessions([sessionRow('sess-alpha', 'ready', 'indexed'), sessionRow('sess-bravo', 'ready', 'indexed'),
      sessionRow('sess-delta', 'ready', 'indexed'), sessionRow('sess-echo', 'not-imported', 'pending')]);
    await settle();
    // I1(2): alpha reverts to not imported.
    await drawSessions([sessionRow('sess-alpha', 'not-imported', 'pending'), sessionRow('sess-bravo', 'ready', 'indexed'),
      sessionRow('sess-delta', 'ready', 'indexed'), sessionRow('sess-echo', 'not-imported', 'pending')]);
    await settle();
    // I1(3): with no input for over 500 ms, data selects a project.
    await pause(650);
    await js(() => { document.querySelector('[data-motion-key="proj-two"]').setAttribute('aria-current', 'true'); });
    await settle();
    // H7(a): with no input for over 500 ms, the project rows move 40 px.
    await pause(650);
    await js(() => { const s = document.createElement('div'); s.style.height = '40px'; document.getElementById('plist').prepend(s); });
    await settle();
    // H7(c): the reader reads proj-one's rect, the rows swap, the press lands on proj-two.
    const aimed = await js(() => window.__uiBench.q('rect', 'newest-project'));
    await js(() => { const l = document.getElementById('plist'); l.append(l.querySelector('[data-motion-key="proj-one"]')); });
    await settle();
    await press({ x: aimed.x + aimed.w / 2, y: aimed.y + Math.min(aimed.h / 2, 10) });
    await settle();
    // I1(4): delta (indexed) shows "Nothing readable yet" while importing, then content.
    await js(() => { document.getElementById('stagecol').innerHTML = '<div class="tr-stage" data-session-key="sess-delta" data-ready="true"><div class="tr-surface"><div class="ah-empty">Nothing readable yet</div></div></div>'; });
    await settle();
    await js(() => { document.querySelector('.tr-stage').innerHTML = '<div class="tr-surface"><div class="tr-sitting" data-sitting-key="s1">content</div></div>'; });
    await settle();
    // I1(7): echo (not imported) is opened; its face names a course the run is not on.
    await press(await centre('[data-motion-key="sess-echo"]'));
    await settle();
    await js(() => { document.getElementById('stagecol').innerHTML = '<div class="tr-stage" data-session-key="sess-echo" data-ready="true"><div data-import-face="not-imported" data-import-course="resumed">Not imported yet</div></div>'; });
    await settle();
    await poll('finalizing');
    await settle();

    const ok = await js(async () => window.__uiBench.q('finish', { daemon: { roots: [['sess-alpha', 'proj-one', 'indexed']], truncated: false }, nonce: 'nonce-1' }));
    let result = null;
    for (let i = 0; i < 100 && result === null; i += 1) {
      const emitted = await js(() => window.__emitted.map((m) => JSON.parse(m)));
      result = emitted.find((m) => m.e === 'result') ?? null;
      if (result === null) await pause(50);
    }
    const events = await js(() => window.__emitted.map((m) => JSON.parse(m).e));
    console.log(`PROBE-DOM ${JSON.stringify({ ok, nonce: result?.nonce ?? null, result: result?.data ?? null, events })}`);
  } catch (error) { failed = true; console.error(error); }
  finally { clearTimeout(deadline); win?.destroy(); rmSync(temporary, { recursive: true, force: true }); app.exit(failed ? 1 : 0); }
}
void run();
