// The Electron main entry for a ui-bench run: the production app
// (clients/app/electron/main.mjs, unchanged) under harness control. run_ui.mjs
// launches `electron --force-device-scale-factor=N tools/ui-bench/app-entry.mjs
// --frame 1500x940` with AH_DAEMON=<origin> (so the app spawns no daemon of its own)
// and the run's private config in UI_BENCH_CONFIG (JSON, env only, never on disk).
// The scale factor must be on Electron's own argv: appendSwitch here is too late.
// In order:
//   1. userData (and session data, crash dumps, logs, temp) point at the run's fresh
//      directory BEFORE main.mjs is imported: it takes the single-instance lock at
//      import, so this never collides with the user's own Agent History app;
//   2. throttling off for every renderer, occlusion included, and the page's focus
//      emulated, since macOS will not activate a background-launched app (declared
//      deviations);
//   3. the host ports that would touch the snapshot's cwds, a browser or the network
//      are replaced by denials (worktree/git, file browsers and browser preview,
//      favicons, shell.openExternal), and the default session cancels every request
//      that is not the app's own scheme, its renderer files, data:/blob: or the
//      harness's daemon: no git, walker, browser or site fetch ever runs on a path or
//      host the frozen snapshot names (each denial is counted);
//   4. the first rosetta://app load waits until the debugger is attached, the
//      binding exists and the probe is registered for every new document (the
//      window first loads about:blank, because a WebContents answers CDP only once
//      it has a renderer); the binding is honoured only from the main frame's own
//      context, so an artifact's srcdoc frame cannot speak for the probe;
//   5. the reader script runs here, driving trusted input through CDP Input.*;
//   6. the probe's export, clock pings and report-only memory peaks go to the
//      driver over the IPC channel; then the app quits. The app also quits if the
//      driver's IPC channel goes away.
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { BrowserWindow, app, ipcMain, screen, session, shell } from 'electron';
import { createReaderContext } from './reader-context.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const mainNow = () => performance.timeOrigin + performance.now();
const wrapperStartAt = mainNow();
const cfg = JSON.parse(process.env.UI_BENCH_CONFIG || '{}');
const send = (msg) => { try { if (process.connected) process.send(msg, () => {}); } catch { /* the driver left */ } };
const log = (msg) => send({ type: 'log', msg: String(msg) });
if (typeof cfg.userData !== 'string' || !path.isAbsolute(cfg.userData)) {
  console.error('ui-bench app-entry: UI_BENCH_CONFIG.userData must be an absolute directory');
  process.exit(2);
}

// 1. A fresh profile, before anything can write the default one.
app.setPath('userData', cfg.userData);
app.setPath('sessionData', path.join(cfg.userData, 'session'));
app.setPath('crashDumps', path.join(cfg.userData, 'crashes'));
app.setAppLogsPath(path.join(cfg.userData, 'logs'));
app.setPath('temp', path.join(cfg.userData, 'tmp'));

// 2. Throttling off for every renderer (occlusion included).
app.commandLine.appendSwitch('disable-renderer-backgrounding');
app.commandLine.appendSwitch('disable-background-timer-throttling');
app.commandLine.appendSwitch('disable-backgrounding-occluded-windows');

// 3. Deny the host ports that would touch the snapshot's cwds, a browser or the network.
const stubbed = { 'ah-worktree-open': 0, 'ah-worktree-git-facts': 0, 'ah-worktree-file-action': 0, 'ah-favicon': 0,
  'ah-file-browsers': 0, 'ah-file-open-browser-preview': 0, 'shell-open-external': 0, 'network-blocked': 0, 'foreign-binding-calls': 0 };
const STUBS = {
  'ah-worktree-open': () => { stubbed['ah-worktree-open'] += 1; return { ok: false, error: 'The worktree column is off in this benchmark run.' }; },
  'ah-worktree-git-facts': () => { stubbed['ah-worktree-git-facts'] += 1; return null; },
  'ah-worktree-file-action': () => { stubbed['ah-worktree-file-action'] += 1; return { ok: false, error: 'off in this benchmark run' }; },
  'ah-favicon': () => { stubbed['ah-favicon'] += 1; return null; },
  'ah-file-browsers': () => { stubbed['ah-file-browsers'] += 1; return []; },
  'ah-file-open-browser-preview': () => { stubbed['ah-file-open-browser-preview'] += 1; return false; },
};
const realHandle = ipcMain.handle.bind(ipcMain);
ipcMain.handle = (channel, listener) => realHandle(channel, STUBS[channel] ?? listener);
shell.openExternal = async () => { stubbed['shell-open-external'] += 1; };
// The default session reaches only the app's own scheme, the renderer files it serves
// from, data:/blob:/about: and the harness's daemon (HTTP and its WebSockets).
const DIST = path.join(here, '..', '..', 'clients', 'app', 'dist') + path.sep;
const daemonHost = (() => { try { return new URL(process.env.AH_DAEMON).host; } catch { return null; } })();
function allowed(raw) {
  let u;
  try { u = new URL(raw); } catch { return false; }
  if (['rosetta:', 'data:', 'blob:', 'about:', 'devtools:'].includes(u.protocol)) return true;
  if (u.protocol === 'file:') { try { return fileURLToPath(u).startsWith(DIST); } catch { return false; } }
  return ['http:', 'ws:'].includes(u.protocol) && daemonHost !== null && u.host === daemonHost;
}
app.whenReady().then(() => {
  session.defaultSession.webRequest.onBeforeRequest((details, callback) => {
    const ok = allowed(details.url);
    if (!ok) stubbed['network-blocked'] += 1;
    callback({ cancel: !ok });
  });
});

// 4. The probe is registered before the first rosetta://app navigation.
const probeSrc = readFileSync(path.join(here, 'probe.js'), 'utf8');
const workerSrc = readFileSync(path.join(here, 'probe-worker.js'), 'utf8');
const prelude = `window.__uiBenchConfig=${JSON.stringify({ giantNative: cfg.giantNative ?? null })};`
  + `window.__uiBenchWorkerSrc=${JSON.stringify(workerSrc)};\n`;
let instrumented = null; // { win, dbg }
const times = { wrapperStartAt, windowAt: null, setupDoneAt: null, didFinishLoadAt: null, scriptStartAt: null, scriptEndAt: null };
const events = new Map(); // probe event name -> { count, first, last }
const watchers = new Set();
let resultResolve;
const resultPromise = new Promise((resolve) => { resultResolve = resolve; });

// The binding is honoured only from the main frame's default execution context (an
// artifact's srcdoc frame shares the binding), and only one result is taken: the one
// carrying this run's nonce, sent in answer to the driver's own finish.
const nonce = Math.random().toString(36).slice(2) + Date.now().toString(36);
let mainFrameId = null;
const contexts = new Map(); // execution context id -> { frameId, isDefault }
let resultTaken = false;
function onProbeEvent(ev) {
  if (ev.e === 'result') {
    if (resultTaken || ev.nonce !== nonce) { log('ignored a probe result without this run\'s nonce'); return; }
    resultTaken = true;
    resultResolve(ev.data);
    return;
  }
  const rec = events.get(ev.e) || { count: 0, first: null, last: null };
  rec.count += 1; rec.last = ev; if (rec.first === null) rec.first = ev;
  events.set(ev.e, rec);
  send({ type: 'event', name: ev.e, t: ev.t });
  for (const w of [...watchers]) w();
}

async function setup(win) {
  const wc = win.webContents;
  wc.setBackgroundThrottling(false);
  win.show();
  app.focus({ steal: true });
  win.focus();
  const dbg = wc.debugger;
  dbg.attach('1.3');
  dbg.on('message', (_event, method, params) => {
    if (method === 'Page.frameNavigated' && !params.frame.parentId) mainFrameId = params.frame.id;
    else if (method === 'Runtime.executionContextCreated') {
      const aux = params.context.auxData || {};
      contexts.set(params.context.id, { frameId: aux.frameId ?? null, isDefault: aux.isDefault === true });
    } else if (method === 'Runtime.executionContextDestroyed') contexts.delete(params.executionContextId);
    else if (method === 'Runtime.executionContextsCleared') contexts.clear();
    else if (method === 'Runtime.bindingCalled' && params.name === '__uiBenchEmit') {
      // Judged at call time, so the order of frameNavigated and contextCreated is moot.
      const c = contexts.get(params.executionContextId);
      if (!c || !c.isDefault || c.frameId !== mainFrameId) { stubbed['foreign-binding-calls'] += 1; return; }
      try { onProbeEvent(JSON.parse(params.payload)); } catch (error) { log(`bad probe payload: ${error.message}`); }
    }
  });
  await dbg.sendCommand('Page.enable');
  mainFrameId = (await dbg.sendCommand('Page.getFrameTree')).frameTree.frame.id;
  await dbg.sendCommand('Runtime.enable');
  // macOS's cooperative activation refuses app.focus({steal}) to a background-launched
  // app, so the page would run unfocused (no focus events, no focus holds). Emulate the
  // focused window a reader has (declared deviation).
  await dbg.sendCommand('Emulation.setFocusEmulationEnabled', { enabled: true });
  await dbg.sendCommand('Runtime.addBinding', { name: '__uiBenchEmit' });
  await dbg.sendCommand('Page.addScriptToEvaluateOnNewDocument', { source: prelude + probeSrc });
  wc.once('did-finish-load', () => { times.didFinishLoadAt = mainNow(); });
  instrumented = { win, dbg };
  times.setupDoneAt = mainNow();
}

const realLoadURL = BrowserWindow.prototype.loadURL;
BrowserWindow.prototype.loadURL = function loadURL(url, options) {
  if (instrumented !== null || typeof url !== 'string' || !url.startsWith('rosetta://app')) {
    return realLoadURL.call(this, url, options);
  }
  times.windowAt = mainNow();
  instrumented = { pending: true };
  return realLoadURL.call(this, 'about:blank').then(() => setup(this)).then(
    () => { startScript(); return realLoadURL.call(this, url, options); },
    (error) => { send({ type: 'fatal', msg: `debugger setup failed: ${error.message}` }); app.exit(3); },
  );
};

// Clock pings from the driver (main ↔ driver offset).
let collectResolve;
const collectAsked = new Promise((resolve) => { collectResolve = resolve; });
process.on('message', (msg) => {
  if (msg?.type === 'ping') send({ type: 'pong', id: msg.id, t: mainNow() });
  else if (msg?.type === 'collect') collectResolve();
  else if (msg?.type === 'quit') quit(msg.reason ?? 'driver');
});
// The driver died or was killed: never outlive it, driving a window on a dead daemon.
process.on('disconnect', () => quit('driver-gone'));

// 5. The reader script, then the export.
let quitting = false;
async function startScript() {
  const { win, dbg } = instrumented;
  const ctx = createReaderContext({ dbg, win, events, watchers, mainNow, log, enroll: cfg.enroll ?? ['claude-code', 'codex'] });
  const heap = { peakMb: null, appPeakMb: null, samples: 0 };
  const sampler = setInterval(async () => {
    try {
      const h = await dbg.sendCommand('Runtime.getHeapUsage');
      const mb = h.usedSize / 2 ** 20;
      if (heap.peakMb === null || mb > heap.peakMb) heap.peakMb = mb;
      const kb = app.getAppMetrics().reduce((sum, m) => sum + (m.memory?.workingSetSize ?? 0), 0);
      if (heap.appPeakMb === null || kb / 1024 > heap.appPeakMb) heap.appPeakMb = kb / 1024;
      heap.samples += 1;
    } catch { /* the page is going away */ }
  }, 1000);
  const state = { ok: false, error: null, offsetsStart: null };
  // The script runs to its end (or fails); collection waits for the driver, which
  // also asks on a timeout, so a run that never finishes is still reported.
  void (async () => {
    try {
      await ctx.untilLoaded();
      state.offsetsStart = await ctx.pingRenderer(20);
      times.scriptStartAt = mainNow();
      const mod = await import(pathToFileURL(cfg.script).href);
      await mod.default.run(ctx);
      state.ok = true;
    } catch (error) {
      state.error = error?.message ?? String(error);
    }
    times.scriptEndAt = mainNow();
    send({ type: 'script-done', ok: state.ok, error: state.error });
  })();
  await collectAsked;
  let probe = null;
  let offsetsEnd = null;
  try {
    offsetsEnd = await ctx.pingRenderer(20);
    const daemon = await daemonRoots();
    await ctx.q('finish', { daemon, nonce });
    probe = await Promise.race([resultPromise, new Promise((r) => setTimeout(() => r(null), 15_000))]);
  } catch (error) {
    log(`collect: ${error?.message ?? error}`);
  }
  clearInterval(sampler);
  const bounds = win.getContentBounds();
  const dpr = screen.getDisplayMatching(win.getBounds()).scaleFactor;
  const display = screen.getDisplayMatching(win.getBounds()).size;
  send({ type: 'result', probe, main: { times, offsets: { start: state.offsetsStart, end: offsetsEnd }, heap, stubbed,
    inputs: ctx.inputLog(), marks: ctx.marks(), window: { contentW: bounds.width, contentH: bounds.height, dpr, displayW: display.width, displayH: display.height },
    eventCounts: Object.fromEntries([...events].map(([k, v]) => [k, v.count])), scriptOk: state.ok, scriptError: state.error } });
}

async function daemonRoots() {
  // End-state truth for I1(5): the daemon's own root list, after the run.
  try {
    const res = await fetch(`${process.env.AH_DAEMON}/api/sessions?limit=1000`);
    const body = await res.json();
    const roots = (body.sessions ?? []).map((s) => [s.id, s.project_id ?? '__rosetta_unassigned__', s.stream_state]);
    return { roots, truncated: roots.length >= 1000 };
  } catch (error) {
    log(`daemon roots: ${error.message}`);
    return null;
  }
}

function quit(reason) {
  if (quitting) return;
  quitting = true;
  log(`quit: ${reason}`);
  app.quit();
  setTimeout(() => app.exit(0), 10_000).unref();
}

// Hand over to the production main process.
await import(pathToFileURL(path.join(here, '..', '..', 'clients', 'app', 'electron', 'main.mjs')).href);
