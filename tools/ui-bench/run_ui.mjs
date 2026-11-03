#!/usr/bin/env node
// One ui-bench run: the production app over a frozen snapshot, read by a versioned
// script, measured by the injected probe, digested read-only, written as ui-run/1.
//
//   node tools/ui-bench/run_ui.mjs --rosettad BIN --snapshot SNAP --out OUT_DIR
//        --script reader-v1|passive-v1 [--keep] [--timeout-s 600] [--dpr 2]
//        [--idle-min 75] [--guard-wait-s 1800] [--no-idle-guard] [--min-free-gb 25]
//
// OUT_DIR must not exist and must lie outside the repository and the snapshot.
// Steps: free-disk and quiet-machine guards; verify_r1_snapshot.js; rosettad `serve
// --home SNAP/home --db OUT/db/rosetta.sqlite --addr 127.0.0.1:0 --json --announce
// --announce-dir OUT/announce --profile` (no --agent: consent is the roll call's; no
// --ticks 0) behind drain.mjs; Electron with app-entry.mjs at a forced device scale
// factor; the script; then quit, SIGINT, the read-only digests and forbidden-value
// list, metrics, the privacy gate, OUT/ui-run.json. The db, the profile and the raw
// logs are deleted unless --keep, on every exit path: success, failure, SIGINT or
// SIGTERM (after every child this run started has stopped). Exit 0 means complete,
// self-test passed, every absolute invariant (I1, H7's jumps and misses) held and the
// privacy gate passed; 1 otherwise; 2 on a harness error; 130/143 on a signal.
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseDaemonLog } from './daemon-lines.mjs';
import { ENROLLED, derive } from './metrics.mjs';
import { SCHEMA_PATH, validateRun } from './privacy.mjs';
import { digestRun, forbiddenFromDb } from './digests.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '..', '..');
const R1 = path.join(REPO, 'tools', 'r1-bench');
const DIST = path.join(REPO, 'clients', 'app', 'dist');
const now = () => performance.timeOrigin + performance.now();
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const tell = (child, msg) => { try { if (child.connected) child.send(msg, () => {}); } catch { /* gone */ } };
export const DEVIATIONS = ['background-throttling-off', 'worktree-ipc-denied', 'favicon-ipc-denied', 'browser-ipc-denied', 'external-links-denied',
  'network-local-only', 'announce-dir-private', 'fresh-profile', 'about-blank-before-load', 'focus-emulated', 'device-scale-factor-forced'];
export const HARNESS_FILES = ['run_ui.mjs', 'app-entry.mjs', 'reader-context.mjs', 'probe.js', 'probe-worker.js', 'drain.mjs', 'daemon-lines.mjs',
  'metrics.mjs', 'privacy.mjs', 'digests.mjs', 'ui-run-1.schema.json', 'scripts/reader-v1.mjs', 'scripts/passive-v1.mjs'];
export const R1_FILES = ['verify_r1_snapshot.js', 'r1_snapshot_lib.js', 'r1_logical_digest.sql', 'r1_cursor_order.sql', 'r1_ranked.py', 'product-diagnostic-v3.json'];

function parseArgs(argv) {
  const a = { keep: false, timeoutS: 600, idleMin: 75, guardWaitS: 1800, idleGuard: true, minFreeGb: 25, dpr: 2 };
  for (let i = 0; i < argv.length; i += 1) {
    const k = argv[i];
    const v = () => argv[++i];
    if (k === '--rosettad') a.bin = v(); else if (k === '--snapshot') a.snap = v(); else if (k === '--out') a.out = v();
    else if (k === '--script') a.script = v(); else if (k === '--keep') a.keep = true; else if (k === '--timeout-s') a.timeoutS = Number(v());
    else if (k === '--idle-min') a.idleMin = Number(v()); else if (k === '--guard-wait-s') a.guardWaitS = Number(v());
    else if (k === '--no-idle-guard') a.idleGuard = false; else if (k === '--min-free-gb') a.minFreeGb = Number(v());
    else if (k === '--dpr') a.dpr = Number(v());
    else throw new Error(`unknown option ${k}`);
  }
  if (!a.bin || !a.snap || !a.out || !['reader-v1', 'passive-v1'].includes(a.script) || !(a.dpr > 0 && a.dpr <= 4)) {
    throw new Error('usage: run_ui.mjs --rosettad BIN --snapshot SNAP --out OUT_DIR --script reader-v1|passive-v1 [--keep] [--dpr N]');
  }
  return a;
}

export const sha256File = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');
/** sha256 over sorted "relpath\tsha256(file)\n" lines of every file under dir. */
export function treeDigest(dir) {
  const lines = [];
  (function walk(d, rel) {
    for (const e of readdirSync(d, { withFileTypes: true }).sort((x, y) => (x.name < y.name ? -1 : 1))) {
      const abs = path.join(d, e.name);
      const r = rel ? `${rel}/${e.name}` : e.name;
      if (e.isDirectory()) walk(abs, r); else if (e.isFile()) lines.push(`${r}\t${sha256File(abs)}\n`);
    }
  })(dir, '');
  return { sha256: createHash('sha256').update(lines.sort().join('')).digest('hex'), files: lines.length };
}
const sysctl = (k) => spawnSync('sysctl', ['-n', k], { encoding: 'utf8' }).stdout.trim();
const swapMb = () => { const m = sysctl('vm.swapusage').match(/used = ([\d.]+)M/); return m ? Number(m[1]) : null; };
function freeGb(dir) {
  const out = spawnSync('df', ['-k', dir], { encoding: 'utf8' }).stdout.trim().split('\n').pop().split(/\s+/);
  return Number(out[3]) / 2 ** 20;
}
function cpuIdle() {
  const out = spawnSync('top', ['-l', '2', '-n', '0', '-s', '2'], { encoding: 'utf8' }).stdout;
  const m = [...out.matchAll(/CPU usage:.*?([\d.]+)% idle/g)].pop();
  return m ? Number(m[1]) : 0;
}
async function quietGuard(a, log) {
  const t0 = Date.now();
  let ok = 0;
  let idle = 0;
  while ((Date.now() - t0) / 1000 < a.guardWaitS) {
    idle = cpuIdle();
    ok = idle >= a.idleMin ? ok + 1 : 0;
    if (ok >= 2) break;
    log(`guard: cpu idle ${idle}% (${ok} of 2 consecutive samples >= ${a.idleMin}%)`);
    await sleep(5000);
  }
  return { cpuIdleAtStart: idle, waitedS: Math.round((Date.now() - t0) / 1000), proceededOverGuard: ok < 2, load1: Math.round(os.loadavg()[0] * 100) / 100 };
}

/** D2(c): the newest big tree's root native id, from the frozen snapshot, in memory only. */
function newestGiant(snap) {
  const lib = createRequire(path.join(R1, 'package.json'))('./r1_snapshot_lib.js');
  const meta = JSON.parse(readFileSync(path.join(snap, 'snapshot.json'), 'utf8'));
  if (!meta.agents['claude-code']) return { native: null, treeBytes: null };
  const { entries } = lib.walkInputs(path.join(snap, 'home'), [{ agent: 'claude-code', dataRoot: meta.agents['claude-code'].dataRoot }]);
  const trees = new Map();
  for (const e of entries) {
    const m = e.rel.match(/^projects\/([^/]+)\/([0-9a-f-]{36})(\.jsonl$|\/)/);
    if (!m) continue;
    const key = `${m[1]}/${m[2]}`;
    const t = trees.get(key) || { native: m[2], k: 0, max: 0, bytes: 0 };
    t.k = Math.max(t.k, e.mtimeMs); t.max = Math.max(t.max, e.size); t.bytes += e.size;
    trees.set(key, t);
  }
  const big = [...trees.values()].filter((t) => t.max > 64 * 2 ** 20).sort((x, y) => y.k - x.k);
  return big.length ? { native: big[0].native, treeBytes: big[0].bytes } : { native: null, treeBytes: null };
}


/** The ui-run/1 document from its parts (pure; the tests build the same way). */
export function buildRunDoc({ script, identity, run, probe, mainNotes, derived, daemonLog, stderr, digests, forbiddenCount }) {
  const inputs = {};
  for (const [, k] of mainNotes?.inputs ?? []) inputs[k] = (inputs[k] ?? 0) + 1;
  const ms = daemonLog.milestones;
  const ta = daemonLog.treeAdmissions;
  const m = derived?.metrics ?? null;
  return {
    format: 'ui-run/1',
    script,
    identity,
    run: {
      ...run, selfTestPass: derived?.selfTest?.pass === true,
      // The absolute invariants: I1 (any failure voids the run) and H7's selection
      // jumps and aim misses (must be 0; a failure is a product finding, not a void).
      invariantsPass: m?.I1?.pass === true && m?.H7?.invariant?.pass === true,
      stubbed: mainNotes?.stubbed ?? null, window: mainNotes?.window ?? null,
      page: probe ? { dpr: probe.page.dpr, innerW: probe.page.innerW, innerH: probe.page.innerH, focusedAtStart: probe.page.focusedAtStart,
        focusedAtRollCall: probe.page.focusedAtRollCall } : null,
      inputs, marks: derived?.scriptMarks ?? [],
    },
    clocks: derived?.clocks ?? null,
    selfTest: derived?.selfTest ?? null,
    daemon: {
      milestonesS: Object.fromEntries(Object.entries(ms).map(([k, v]) => [k, v.elapsedS === null ? null : Math.round(v.elapsedS * 1e4) / 1e4])),
      milestonesMs: derived?.milestonesMs ?? null,
      horizonSessions: Object.fromEntries(Object.entries(ms).filter(([, v]) => v.horizonSessions !== undefined).map(([k, v]) => [k, v.horizonSessions])),
      newestGiantTreeBytes: ms['newest-giant-ready']?.treeBytes ?? null,
      treeAdmission: { batches: ta.length, roots: ta.reduce((t, b) => t + (b.roots ?? 0), 0), sessions: ta.reduce((t, b) => t + (b.sessions ?? 0), 0),
        maxLockHeldMs: Math.max(0, ...ta.map((b) => b.lockHeldMs ?? 0)) },
      memoryPeak: daemonLog.memoryPeak, runtimeReadableSessions: daemonLog.runtimeReadableSessions,
      metadataPublicSessions: daemonLog.metadataPublicSessions, unknownMilestones: daemonLog.unknownMilestones,
      stdoutLines: daemonLog.lines, jsonEvents: daemonLog.jsonEvents, panicLines: daemonLog.panicLines, stderrLines: stderr.length,
      stderrLockedLines: stderr.filter((l) => /SQLITE_LOCKED|database (table )?is locked|table is locked/i.test(l)).length,
    },
    metrics: m,
    equality: { ...digests, hitDigest: m?.H8?.hitDigest ?? null, finalRoots: probe?.end?.rootsSeen ?? null, finalReadySessions: probe?.end?.readyAll ?? null },
    privacy: { schema: 'pending', forbiddenScan: 'pending', forbiddenValues: forbiddenCount },
    probe,
  };
}

/** A promise's value, or `fallback` once `ms` have passed; the timer never outlives it. */
function within(promise, ms, fallback = undefined) {
  let timer;
  const timeout = new Promise((resolve) => { timer = setTimeout(() => resolve(fallback), ms); });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}
const isOutside = (rel) => rel === '..' || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel);

// Children this run started, so neither a failure nor a signal leaves one behind.
const live = { drain: null, electron: null, daemonPid: null, daemonExited: null, daemonDone: true, extra: new Map(), cleanup: null, stopping: false };
function track(child, killSignal) {
  live.extra.set(child, killSignal);
  if (live.stopping) { try { child.kill(killSignal); } catch { /* gone */ } } // spawned as the run was stopping
  return () => live.extra.delete(child);
}
/** Once a signal began the stop, main() starts nothing new. */
function checkStop() { if (live.stopping) throw new Error('stopping'); }
/** Stop every child this run started, the daemon last-but-surely; true once its exit was seen. */
async function stopChildren(log) {
  if (live.electron && live.electron.exitCode === null && live.electron.signalCode === null) {
    tell(live.electron, { type: 'quit', reason: 'driver-stopping' });
    await within(new Promise((resolve) => live.electron.once('exit', resolve)), 15_000);
    if (live.electron.exitCode === null && live.electron.signalCode === null) live.electron.kill('SIGKILL');
  }
  // A digest child (r1_ranked.py and the warm daemon it reaps on SIGINT, sqlite3,
  // ledger verify): its own stop signal, time to clean up, then SIGKILL.
  const extras = [...live.extra];
  for (const [child, sig] of extras) { try { child.kill(sig); } catch { /* gone */ } }
  await Promise.all(extras.map(async ([child]) => {
    if (child.exitCode !== null || child.signalCode !== null) return;
    await within(new Promise((resolve) => child.once('close', resolve)), 200_000);
    if (child.exitCode === null && child.signalCode === null) { try { child.kill('SIGKILL'); } catch { /* gone */ } }
  }));
  if (!live.daemonDone && live.drain) {
    tell(live.drain, { type: 'stop', graceMs: 60_000 });
    await within(live.daemonExited, 75_000);
    if (!live.daemonDone && live.daemonPid !== null) {
      log('daemon did not stop; SIGKILL to its pid');
      try { process.kill(live.daemonPid, 'SIGKILL'); } catch { /* gone */ }
      await within(live.daemonExited, 15_000);
    }
  }
  if (live.drain && live.drain.exitCode === null && live.drain.signalCode === null && live.daemonDone) {
    await within(new Promise((resolve) => live.drain.once('exit', resolve)), 5_000);
  }
  return live.daemonDone;
}

async function main() {
  const a = parseArgs(process.argv.slice(2));
  const out = path.resolve(a.out);
  const snap = path.resolve(a.snap);
  if (!isOutside(path.relative(REPO, out))) throw new Error('OUT_DIR must be outside the repository');
  if (!isOutside(path.relative(snap, out))) throw new Error('OUT_DIR must be outside the snapshot');
  if (existsSync(out)) throw new Error('OUT_DIR exists; refusing');
  const bin = path.resolve(a.bin);
  const scriptPath = path.join(HERE, 'scripts', `${a.script}.mjs`);
  if (!existsSync(path.join(DIST, 'index.html'))) throw new Error('no renderer build: run `npm run build -w @vibefield/rosetta-agent-history` first');
  mkdirSync(out, { mode: 0o700 });
  const logLines = [];
  const log = (msg) => { const line = `${new Date().toISOString()} ${msg}`; logLines.push(line); console.log(`ui-bench: ${msg}`); };
  const writeLog = () => { try { writeFileSync(path.join(out, 'run.log'), `${logLines.join('\n')}\n`, { mode: 0o600 }); } catch { /* best effort */ } };
  // What a run leaves behind without --keep: ui-run.json, digests/ and run.log only.
  // The db is removed only once the daemon's exit was seen (never under a live writer).
  live.cleanup = (daemonStopped) => {
    if (a.keep) { writeLog(); return; }
    const doomed = ['user-data', 'announce', 'stdout.log', 'stderr.log', 'electron.log', 'digest-errors.log', 'privacy-errors.txt', 'ui-run.rejected.json'];
    if (daemonStopped && live.extra.size === 0) doomed.push('db');
    else log('the daemon or a digest child may still hold the db; it was kept');
    for (const p of doomed) rmSync(path.join(out, p), { recursive: true, force: true });
    writeLog();
  };
  const free0 = freeGb(out);
  if (free0 < a.minFreeGb) throw new Error(`free disk ${free0.toFixed(1)} GiB < ${a.minFreeGb}`);
  const guard = a.idleGuard ? await quietGuard(a, log) : { cpuIdleAtStart: cpuIdle(), waitedS: 0, proceededOverGuard: true, load1: Math.round(os.loadavg()[0] * 100) / 100 };
  log(`guard: idle ${guard.cpuIdleAtStart}% waited ${guard.waitedS}s`);

  // Identity, then the snapshot check right before the run.
  const verify = spawnSync(process.execPath, [path.join(R1, 'verify_r1_snapshot.js'), snap], { encoding: 'utf8', maxBuffer: 1 << 24 });
  const verified = JSON.parse(verify.stdout.trim().split('\n').pop() || '{}');
  if (verify.status !== 0 || verified.ok !== true) throw new Error(`snapshot verification failed: ${(verified.problems || []).length} problems`);
  const snapMeta = JSON.parse(readFileSync(path.join(snap, 'snapshot.json'), 'utf8'));
  const dist = treeDigest(DIST);
  const harnessSha256 = {};
  for (const f of HARNESS_FILES) harnessSha256[f] = sha256File(path.join(HERE, f));
  for (const f of R1_FILES) harnessSha256[`r1-bench/${f}`] = sha256File(path.join(R1, f));
  const scriptMod = await import(pathToFileURL(scriptPath).href);
  const giant = newestGiant(snap);
  const electronBin = createRequire(path.join(REPO, 'clients', 'app', 'package.json'))('electron');
  const electronVersion = JSON.parse(readFileSync(path.join(REPO, 'node_modules', 'electron', 'package.json'), 'utf8')).version;
  const identity = {
    rosettadSha256: sha256File(bin), distSha256: dist.sha256, distFiles: dist.files, electron: electronVersion,
    snapshot: { walkInputsDigest: snapMeta.walkInputsDigest, manifestSha256: snapMeta.manifestSha256, files: snapMeta.totals?.files ?? verified.files, bytes: snapMeta.totals?.bytes ?? verified.bytes },
    harnessSha256, machine: { model: sysctl('hw.model'), os: spawnSync('sw_vers', ['-productVersion'], { encoding: 'utf8' }).stdout.trim(), cpus: os.cpus().length, memGb: Math.round(os.totalmem() / 2 ** 30) },
    windowPx: '1500x940', dpr: a.dpr, dprMeasured: null, deviations: DEVIATIONS, enrolled: ENROLLED,
  };

  // The daemon, behind its dedicated reader.
  for (const d of ['db', 'user-data', 'announce']) mkdirSync(path.join(out, d), { mode: 0o700 });
  const db = path.join(out, 'db', 'rosetta.sqlite');
  const serve = ['serve', '--home', path.join(snap, 'home'), '--db', db, '--addr', '127.0.0.1:0', '--json', '--announce',
    '--announce-dir', path.join(out, 'announce'), '--profile'];
  const swap0 = swapMb();
  const startedAt = new Date().toISOString();
  const runT0 = now();
  live.daemonDone = false;
  const drain = live.drain = spawn(process.execPath, [path.join(HERE, 'drain.mjs'), path.join(out, 'stdout.log'), path.join(out, 'stderr.log'), '--', bin, ...serve],
    { stdio: ['ignore', 'inherit', 'inherit', 'ipc'] });
  const daemon = { origin: null, milestones: new Set(), exit: null };
  let readyResolve;
  const ready = new Promise((resolve) => { readyResolve = resolve; });
  let daemonExitResolve;
  live.daemonExited = new Promise((resolve) => { daemonExitResolve = resolve; });
  const daemonGone = (exit) => { if (daemon.exit === null) daemon.exit = exit; live.daemonDone = true; daemonExitResolve(); readyResolve(); };
  drain.on('message', (m) => {
    if (m.type === 'spawned') live.daemonPid = m.pid ?? null;
    else if (m.type === 'line') {
      if (m.line.startsWith('{') && m.line.includes('"event":"ready"')) { try { daemon.origin = JSON.parse(m.line).origin; readyResolve(); } catch { /* not yet */ } }
      const ms = m.line.match(/^milestone: (\S+)/);
      if (ms) daemon.milestones.add(ms[1]);
    } else if (m.type === 'exit') daemonGone({ code: m.code, signal: m.signal });
  });
  // The drain closes only after the daemon's pipes did; if it dies without saying so,
  // the daemon may still run: only an 'exit' message (or its pid gone) counts.
  drain.on('exit', () => {
    if (live.daemonDone) return;
    let alive = live.daemonPid !== null;
    try { if (alive) process.kill(live.daemonPid, 0); } catch { alive = false; }
    if (!alive) daemonGone({ code: null, signal: null });
  });
  await within(ready, 120_000);
  checkStop();
  if (daemon.origin === null) throw new Error('rosettad never reported ready');
  log('daemon ready');

  // The app, at a fixed device scale factor (on Electron's own argv; macOS honours it).
  const elog = openSync(path.join(out, 'electron.log'), 'wx', 0o600);
  const config = { userData: path.join(out, 'user-data'), script: scriptPath, giantNative: giant.native, enroll: ENROLLED };
  const electron = live.electron = spawn(electronBin, [`--force-device-scale-factor=${a.dpr}`, path.join(HERE, 'app-entry.mjs'), '--frame', '1500x940'], {
    stdio: ['ignore', elog, elog, 'ipc'], env: { ...process.env, AH_DAEMON: daemon.origin, UI_BENCH_CONFIG: JSON.stringify(config) },
  });
  const app = { scriptDone: null, result: null, exit: null, pongs: new Map(), fatal: null };
  let appExitResolve;
  const appExited = new Promise((resolve) => { appExitResolve = resolve; });
  electron.on('message', (m) => {
    if (m.type === 'pong') app.pongs.get(m.id)?.(m.t);
    else if (m.type === 'script-done') { app.scriptDone = m; log(`script done ok=${m.ok}${m.error ? ` (${m.error})` : ''}`); }
    else if (m.type === 'result') app.result = m;
    else if (m.type === 'fatal') { app.fatal = m.msg; log(`app fatal: ${m.msg}`); }
    else if (m.type === 'log') log(`app: ${m.msg}`);
    else if (m.type === 'event' && ['first-meta', 'cols-up', 'frozen', 'newest-ready', 'all-readable', 'status-canonical', 'status-search', 'portfolio'].includes(m.name)) log(`probe: ${m.name}`);
  });
  electron.on('exit', (code) => { app.exit = code ?? -1; appExitResolve(); });
  electron.on('error', (error) => log(`electron: ${error.message}`));
  drain.on('error', (error) => log(`drain: ${error.message}`));
  let pingSeq = 0;
  async function pingMain(n) {
    let best = null;
    for (let i = 0; i < n; i += 1) {
      const id = (pingSeq += 1);
      const t1 = now();
      const t = await within(new Promise((resolve) => { app.pongs.set(id, resolve); tell(electron, { type: 'ping', id }); }), 2000, null);
      const t2 = now();
      app.pongs.delete(id);
      if (t === null) continue;
      if (best === null || t2 - t1 < best.rttMs) best = { offsetMs: t - (t1 + t2) / 2, rttMs: t2 - t1 };
    }
    return best;
  }
  await new Promise((resolve) => setTimeout(resolve, 1500));
  const pingsStart = await pingMain(20);
  log(`clock: main offset ${pingsStart?.offsetMs?.toFixed(3)} ms, rtt ${pingsStart?.rttMs?.toFixed(3)} ms`);

  // Run until the script is done and the daemon has published and searched, or time out.
  const deadline = runT0 + a.timeoutS * 1000;
  let outcome = 'complete';
  const reasons = [];
  while (true) {
    const searched = daemon.milestones.has('search-ready') || daemon.milestones.has('search-failed');
    if (app.scriptDone && daemon.milestones.has('canonical-ready') && searched) break;
    if (app.exit !== null || daemon.exit !== null || app.fatal) { outcome = 'failed'; reasons.push(app.fatal ? 'app-fatal' : app.exit !== null ? 'app-exited' : 'daemon-exited'); break; }
    if (now() > deadline) { outcome = 'timeout'; break; }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  if (outcome === 'complete' && app.scriptDone?.ok === false) { outcome = 'failed'; reasons.push('script-error'); }
  log(`run ${outcome}`);
  const pingsEnd = app.exit === null ? await pingMain(20) : null;
  if (app.exit === null) tell(electron, { type: 'collect' });
  const t1 = now();
  while (app.result === null && app.exit === null && now() - t1 < 30_000) await new Promise((resolve) => setTimeout(resolve, 100));
  if (app.exit === null) tell(electron, { type: 'quit' });
  await within(appExited, 20_000);
  if (app.exit === null) { log('electron did not quit; SIGKILL'); electron.kill('SIGKILL'); await within(appExited, 5_000); }
  closeSync(elog);
  if (!live.daemonDone) tell(drain, { type: 'stop', graceMs: 60_000 });
  await within(live.daemonExited, 75_000);
  if (!live.daemonDone && live.daemonPid !== null) {
    log('daemon did not stop; SIGKILL to its pid');
    try { process.kill(live.daemonPid, 'SIGKILL'); } catch { /* gone */ }
    await within(live.daemonExited, 15_000);
  }
  if (!live.daemonDone) { outcome = 'failed'; reasons.push('daemon-did-not-stop'); }
  const wallS = (now() - runT0) / 1000;
  const swap1 = swapMb();

  // Read-only digests and the forbidden-value list from the run's own projection,
  // only once the daemon is gone (immutable=1 needs no live writer).
  checkStop();
  const rawLog = path.join(out, 'digest-errors.log');
  const digests = live.daemonDone ? await digestRun({ bin, db, snapHome: path.join(snap, 'home'), outDir: path.join(out, 'digests'), r1: R1, log, track, rawLog })
    : await digestRun({ bin, db: path.join(out, 'no-db'), snapHome: '', outDir: path.join(out, 'digests'), r1: R1, log, track, rawLog });
  const fromDb = live.daemonDone ? await forbiddenFromDb({ db, track }) : { ok: false, values: [], reason: 'daemon-alive' };
  if (!fromDb.ok) log(`privacy: the forbidden-value list could not be read from the db (${fromDb.reason}); the document will not be written`);
  const forbidden = [...new Set([snap, out, bin, os.homedir(), os.userInfo().username, os.hostname(), giant.native, ...(scriptMod.QUERIES ?? []), ...fromDb.values].filter(Boolean))];
  const daemonLog = parseDaemonLog(readFileSync(path.join(out, 'stdout.log'), 'utf8'));
  const stderr = readFileSync(path.join(out, 'stderr.log'), 'utf8').split('\n').filter(Boolean);
  const probe = app.result?.probe ?? null;
  const mainNotes = app.result?.main ?? null;
  const derived = probe ? derive({ probe, main: mainNotes, daemon: daemonLog, mainPings: { start: pingsStart, end: pingsEnd }, notApplicable: scriptMod.NOT_APPLICABLE ?? [] }) : null;
  identity.dprMeasured = probe?.page?.dpr ?? mainNotes?.window?.dpr ?? null;
  if (identity.dprMeasured !== a.dpr || (mainNotes?.window?.dpr != null && mainNotes.window.dpr !== a.dpr)) {
    if (outcome === 'complete') outcome = 'failed';
    reasons.push('dpr-mismatch');
  }
  // A step that timed out leaves its metric unmeasured: the run is incomplete, not complete.
  const timeouts = (derived?.scriptMarks ?? []).filter(([, name]) => name.startsWith('timeout-')).length;
  if (timeouts > 0 && outcome === 'complete') { outcome = 'incomplete'; reasons.push('script-step-timeout'); }
  const run = buildRunDoc({
    script: { name: a.script, sha256: sha256File(scriptPath) }, identity, probe, mainNotes, derived, daemonLog, stderr, digests,
    forbiddenCount: forbidden.length,
    run: {
      startedAt, outcome, outcomeReasons: [...new Set(reasons)], wallS: Math.round(wallS * 10) / 10,
      daemonExit: { code: daemon.exit?.code ?? null, signal: daemon.exit?.signal ?? null }, electronExit: app.exit,
      guard: { ...guard, idleMin: a.idleMin, swapMbBefore: swap0, swapMbAfter: swap1, freeGbBefore: Math.round(free0 * 10) / 10 },
      snapshotVerify: { ok: verified.ok, files: verified.files, bytes: verified.bytes, wallMs: verified.wallMs },
      scriptOk: app.scriptDone?.ok === true, scriptTimeouts: timeouts, newestGiantKnown: giant.native !== null, newestGiantTreeBytes: giant.treeBytes,
    },
  });
  const gate = () => {
    try {
      const v = validateRun(run, { forbidden });
      if (!fromDb.ok) { v.ok = false; v.errors.push('the forbidden-value list from the run db is unavailable'); }
      return v;
    } catch (error) { return { ok: false, errors: ['privacy gate could not run'] }; }
  };
  const verdict = gate();
  run.privacy = { schema: verdict.ok ? 'pass' : 'fail', forbiddenScan: verdict.ok ? 'pass' : 'fail', forbiddenValues: forbidden.length,
    forbiddenSource: fromDb.ok ? 'run-db' : 'unavailable', schemaSha256: existsSync(SCHEMA_PATH) ? sha256File(SCHEMA_PATH) : null };
  if (run.metrics) run.metrics.I1.checks.c6Privacy = { violations: verdict.ok ? 0 : 1, exercised: 1, pass: verdict.ok };
  const recheck = gate();
  const ok = verdict.ok && recheck.ok;
  if (!ok) {
    // The errors name paths and key hashes, never a refused value (privacy.mjs).
    for (const e of [...verdict.errors, ...recheck.errors].slice(0, 20)) log(`privacy: ${e}`);
    log(`privacy gate FAILED (${verdict.errors.length} errors); ui-run.json not written`);
    // Debugging only: with --keep, the rejected document stays beside the private db.
    if (a.keep) {
      writeFileSync(path.join(out, 'privacy-errors.txt'), `${[...verdict.errors, ...recheck.errors].join('\n')}\n`, { mode: 0o600 });
      writeFileSync(path.join(out, 'ui-run.rejected.json'), `${JSON.stringify(run, null, 1)}\n`, { mode: 0o600 });
    }
  } else {
    writeFileSync(path.join(out, 'ui-run.json'), `${JSON.stringify(run, null, 1)}\n`, { flag: 'wx', mode: 0o644 });
  }
  const m = run.metrics;
  log(`done ${outcome}${reasons.length ? ` (${[...new Set(reasons)].join(', ')})` : ''} selfTest=${run.selfTest?.pass} I1=${m?.I1?.pass} H7inv=${m?.H7?.invariant?.pass} `
    + `logical=${(digests.logicalTotal ?? '').slice(0, 12)} sourceSet=${(digests.sourceSetDigest ?? '').slice(0, 12)} ranked=${(digests.rankedIdsSha256 ?? '').slice(0, 12)}`);
  writeLog();
  return outcome === 'complete' && ok && run.selfTest?.pass === true && run.run.invariantsPass ? 0 : 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  let stopping = false;
  const finish = async (code, why) => {
    if (stopping) return;
    stopping = true;
    live.stopping = code === 130 || code === 143;
    if (why) console.error(`ui-bench: ${why}`);
    const quietLog = (msg) => console.error(`ui-bench: ${msg}`);
    const daemonStopped = await stopChildren(quietLog);
    try { live.cleanup?.(daemonStopped); } catch (error) { console.error(`ui-bench: cleanup: ${error.message}`); }
    process.exit(code);
  };
  // A signal aimed at this process alone (an orchestrator's timeout, `kill -INT pid`)
  // still stops Electron, the drain's daemon and any digest child, and cleans up.
  process.once('SIGINT', () => { void finish(130, 'SIGINT: stopping the run'); });
  process.once('SIGTERM', () => { void finish(143, 'SIGTERM: stopping the run'); });
  main().then((code) => finish(code, null), (error) => finish(2, error.message));
}
