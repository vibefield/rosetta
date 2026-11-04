#!/usr/bin/env node
'use strict';
// One R1 cold run over the frozen snapshot (brief v3 §12 E3), from
// imgdefer/run_cold.js. Timed like the ingest-bench harness
// (tools/ingest-bench/src/lib/daemon.js executeRosettad: node spawn -> close,
// line-decoded stdout/stderr, JSON events parsed from stdout), reproduced
// inline with parseProfile verbatim so the harness never depends on a
// checkout's working tree. /usr/bin/time -l wraps the daemon for memory.
//
//   rosettad serve --home SNAP/home --agent claude-code --agent codex --ticks 0
//     --addr 127.0.0.1:0 --json --profile --db OUT/db/rosetta.sqlite [diag flags]
//
// usage: node run_r1.js BINARY SNAPSHOT_DIR OUT_DIR [--label L] [--load] [--agent K ...] [-- EXTRA_SERVE_FLAGS...]
//   --load    fork r1_reader_load.js against the bound address (P3)
//   --agent   override the agent set (default claude-code + codex)
// Writes OUT/stdout.log, OUT/stderr.log (private; pair_r1.sh deletes them with
// the db once digested) and OUT/report.json (sanitized: no paths, no content).

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync, fork } = require('node:child_process');
const { createHash } = require('node:crypto');

const argv = process.argv.slice(2);
const dash = argv.indexOf('--');
const extra = dash >= 0 ? argv.slice(dash + 1) : [];
const own = dash >= 0 ? argv.slice(0, dash) : argv;
const [binary, snapshot, outDir, ...opts] = own;
let label = null;
let load = false;
const agents = [];
for (let i = 0; i < opts.length; i += 1) {
  if (opts[i] === '--label') label = opts[++i];
  else if (opts[i] === '--load') load = true;
  else if (opts[i] === '--agent') agents.push(opts[++i]);
  else { console.error(`unknown option ${opts[i]}`); process.exit(2); }
}
if (!binary || !snapshot || !outDir) {
  console.error('usage: run_r1.js BINARY SNAPSHOT_DIR OUT_DIR [--label L] [--load] [--agent K ...] [-- EXTRA...]');
  process.exit(2);
}
if (!agents.length) agents.push('claude-code', 'codex');
if (fs.existsSync(outDir)) { console.error('output directory exists; refusing'); process.exit(2); }
const home = path.join(snapshot, 'home');
const snapMeta = JSON.parse(fs.readFileSync(path.join(snapshot, 'snapshot.json'), 'utf8'));
fs.mkdirSync(outDir, { recursive: false, mode: 0o700 });
const dbDir = path.join(outDir, 'db');
fs.mkdirSync(dbDir, { mode: 0o700 });
const db = path.join(dbDir, 'rosetta.sqlite');
const ledger = db.replace(/\.sqlite$/, '.ledger'); // db.with_extension("ledger")
const args = ['serve', '--home', home, ...agents.flatMap((a) => ['--agent', a]), '--ticks', '0',
  '--addr', '127.0.0.1:0', '--json', '--profile', '--db', db, ...extra];

// --- verbatim from tools/ingest-bench/src/lib/daemon.js ------------------
function parseProfile(report) {
  if (typeof report !== 'string') return {};
  const metrics = {};
  for (const line of report.split(/\r?\n/)) {
    const match = line.match(/^(.+?)\s+(-?\d+(?:\.\d+)?)(?:\s+(ms|MiB\/s|records\/s|ops\/s))?$/);
    if (!match) continue;
    metrics[match[1].trim()] = {
      value: Number(match[2]),
      unit: match[3] || 'count',
    };
  }
  return metrics;
}
function lineDecoder(onLine) {
  let pending = '';
  return {
    push(chunk) {
      pending += chunk.toString('utf8');
      const lines = pending.split(/\r?\n/);
      pending = lines.pop() || '';
      for (const line of lines) if (line) onLine(line);
    },
    end() {
      if (pending) onLine(pending);
      pending = '';
    },
  };
}
// -------------------------------------------------------------------------

const scrub = (line) => line
  .split(outDir).join('<OUT>')
  .split(path.resolve(outDir)).join('<OUT>')
  .split(snapshot).join('<SNAPSHOT>')
  .split(path.resolve(snapshot)).join('<SNAPSHOT>')
  .split(process.env.HOME || '\u0000').join('<HOME>');
function sha256File(file) {
  const h = createHash('sha256');
  h.update(fs.readFileSync(file));
  return h.digest('hex');
}
function machine() {
  const sysctl = (k) => spawnSync('sysctl', ['-n', k], { encoding: 'utf8' }).stdout.trim();
  const swap = sysctl('vm.swapusage');
  const used = swap.match(/used = ([\d.]+)M/);
  const load1 = os.loadavg()[0];
  return { at: new Date().toISOString(), loadavg: os.loadavg().map((v) => Math.round(v * 100) / 100),
    load1: Math.round(load1 * 100) / 100, swapUsedMb: used ? Number(used[1]) : null, freeMemMb: Math.round(os.freemem() / 2 ** 20) };
}
function treeBytes(root) {
  if (!fs.existsSync(root)) return 0;
  const st = fs.lstatSync(root);
  if (!st.isDirectory()) return st.size;
  return fs.readdirSync(root).reduce((s, e) => s + treeBytes(path.join(root, e)), 0);
}
function treeFiles(root) {
  if (!fs.existsSync(root)) return 0;
  const st = fs.lstatSync(root);
  if (!st.isDirectory()) return 1;
  return fs.readdirSync(root).reduce((s, e) => s + treeFiles(path.join(root, e)), 0);
}

const stdoutLog = fs.openSync(path.join(outDir, 'stdout.log'), 'wx', 0o600);
const stderrLog = fs.openSync(path.join(outDir, 'stderr.log'), 'wx', 0o600);
const textLines = [];
const stderrLines = [];
const events = [];
let origin = null;
let readyMs = null;
let reader = null;
let readerExit = null;
let phase = 'meta';
const phaseAt = { meta: 0 };
const runtimesReadable = new Set();

function setPhase(next, ms) {
  if (phase === next) return;
  phase = next;
  phaseAt[next] = Number(ms.toFixed(1));
  if (reader && reader.connected) reader.send({ type: 'phase', phase: next, ms });
}

function onText(line, ms) {
  if (line.startsWith('milestone: metadata-ready')) setPhase('streaming', ms);
  const rr = line.match(/^milestone: runtime-readable runtime=\S+ agent=(\S+)/);
  if (rr) {
    runtimesReadable.add(rr[1]);
    if (agents.every((a) => runtimesReadable.has(a))) setPhase('finalize', ms);
  }
  if (line.startsWith('milestone: canonical-ready')) setPhase('post', ms);
}

function startReader() {
  const script = path.join(__dirname, 'r1_reader_load.js');
  reader = fork(script, [origin, path.join(outDir, 'reader.json')], { stdio: ['ignore', 'ignore', 'inherit', 'ipc'] });
  reader.on('exit', (code) => { readerExit = code; });
  reader.send({ type: 'phase', phase, ms: Number(process.hrtime.bigint() - t0) / 1e6 });
}

const before = machine();
const binarySha256 = sha256File(binary);
const startedAt = new Date().toISOString();
const t0 = process.hrtime.bigint();
const child = spawn('/usr/bin/time', ['-l', binary, ...args], { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
const consume = (stream, line) => {
  const ms = Number(process.hrtime.bigint() - t0) / 1e6;
  fs.writeSync(stream === 'stdout' ? stdoutLog : stderrLog, `${ms.toFixed(1)}\t${line}\n`);
  if (stream === 'stderr') { stderrLines.push(line); return; }
  if (line.startsWith('{')) {
    let event;
    try { event = JSON.parse(line); } catch { textLines.push({ ms: Number(ms.toFixed(1)), line: scrub(line) }); return; }
    event.__ms = Number(ms.toFixed(1));
    events.push(event);
    if (event.event === 'ready' && !origin) {
      origin = event.origin;
      readyMs = Number(ms.toFixed(1));
      if (load) startReader();
    }
    return;
  }
  textLines.push({ ms: Number(ms.toFixed(1)), line: scrub(line) });
  onText(line, ms);
};
const out = lineDecoder((line) => consume('stdout', line));
const err = lineDecoder((line) => consume('stderr', line));
child.stdout.on('data', (chunk) => out.push(chunk));
child.stderr.on('data', (chunk) => err.push(chunk));
child.once('error', (error) => { console.error(`could not start rosettad: ${error.message}`); process.exitCode = 1; });
child.once('close', async (code, signal) => {
  out.end();
  err.end();
  const processWallMs = Number(process.hrtime.bigint() - t0) / 1e6;
  fs.closeSync(stdoutLog);
  fs.closeSync(stderrLog);
  if (reader) {
    if (reader.connected) reader.send({ type: 'stop', ms: processWallMs });
    const deadline = Date.now() + 30_000;
    while (readerExit === null && Date.now() < deadline) await new Promise((r) => setTimeout(r, 50));
    if (readerExit === null) reader.kill('SIGKILL');
  }
  const after = machine();
  const time = {};
  for (const line of stderrLines) {
    let m = line.match(/^\s*([\d.]+)\s+real\s+([\d.]+)\s+user\s+([\d.]+)\s+sys/);
    if (m) Object.assign(time, { realS: +m[1], userS: +m[2], sysS: +m[3] });
    m = line.match(/^\s*(\d+)\s+(maximum resident set size|peak memory footprint|page reclaims|page faults|involuntary context switches|voluntary context switches|instructions retired|cycles elapsed)$/);
    if (m) time[m[2]] = +m[1];
  }
  const profiles = events.filter((e) => e.event === 'profile')
    .map((e) => ({ command: e.command, ms: e.__ms, metrics: parseProfile(e.report) }));
  const stages = events.filter((e) => e.event === 'stage-profile').map((e) => {
    const { event: _e, report, ...rest } = e;
    const s = JSON.parse(scrub(JSON.stringify(rest)));
    if (report !== undefined) s.metrics = parseProfile(report);
    return s;
  });
  const otherEvents = events.filter((e) => !['profile', 'stage-profile', 'source'].includes(e.event))
    .map((e) => JSON.parse(scrub(JSON.stringify(e))));
  let readerReport = null;
  const readerPath = path.join(outDir, 'reader.json');
  if (fs.existsSync(readerPath)) readerReport = JSON.parse(fs.readFileSync(readerPath, 'utf8'));
  const report = {
    format: 'r1-cold-run/1',
    label,
    startedAt,
    binarySha256,
    snapshot: { walkInputsDigest: snapMeta.walkInputsDigest, manifestSha256: snapMeta.manifestSha256 },
    command: ['rosettad', ...args].map(scrub),
    extraFlags: extra,
    wrapper: '/usr/bin/time -l (memory only; wall = spawn->close, as ingest-bench executeRosettad)',
    machineBefore: before,
    machineAfter: after,
    exitCode: code,
    signal,
    processWallMs: Number(processWallMs.toFixed(1)),
    readyMs,
    phaseAt,
    time,
    textLines,
    stages,
    profiles,
    otherEvents,
    reader: readerReport,
    readerExit,
    outputs: {
      sqliteBytes: treeBytes(db),
      ledgerBytes: treeBytes(ledger),
      searchSidecarBytes: treeBytes(`${db}.search`),
      assetsDirBytes: treeBytes(`${db}.assets`),
      assetsDirFiles: treeFiles(`${db}.assets`),
    },
    stderrLineCount: stderrLines.length,
    // Counts only (Task C): SQLITE_LOCKED evidence on the daemon's stderr, and panics.
    stderrLockedLines: stderrLines.filter((l) => /SQLITE_LOCKED|database (table )?is locked|table is locked/i.test(l)).length,
    stderrPanicLines: stderrLines.filter((l) => /panicked at|thread '.*' panicked/i.test(l)).length,
  };
  fs.writeFileSync(path.join(outDir, 'report.json'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  const cr = textLines.find((t) => t.line.startsWith('milestone: canonical-ready'));
  console.log(`${label || 'run'} exit=${code} wall_ms=${report.processWallMs} cr=${cr ? cr.line.split('elapsed_s=')[1] : 'none'}`
    + ` peak_footprint=${time['peak memory footprint']} load1=${before.load1} swap_mb=${before.swapUsedMb}->${after.swapUsedMb}`);
  if (code !== 0) process.exitCode = 1;
});
