#!/usr/bin/env node
// The ui-bench pair runner: one frozen BEFORE/AFTER pair (or an A/A campaign) of
// run_ui.mjs runs, interleaved, then pair-eval.mjs's verdict. run_ui.mjs is unchanged;
// this file only schedules it.
//
//   node tools/ui-bench/pair.mjs side --rosettad BIN --dist DIR (--snapshot SNAP | --home DIR --fixture MANIFEST)
//        [--scripts reader-v1,passive-v1] [--runs 11] [run options] > SIDE.json
//   node tools/ui-bench/pair.mjs run --before A.json --after B.json --out OUT [--kind daemon|ux]
//        [--claims CLAIMS.json] [--floors AA_OUT/ui-pair.json] [--order abab|abba] [--dry-run]
//   node tools/ui-bench/pair.mjs run --aa A.json --out OUT [--order abab|abba] [--dry-run]
//
// A side file names the rosettad binary and the renderer dist with their sha256 (the dist
// as run_ui.mjs hashes it: treeDigest), one source (the frozen snapshot, or a fixture home
// and its manifest), the scripts, N runs per side and run_ui's options. Before any run the
// pair is refused (exit 2) when the sides' source, scripts, runs or options differ, when
// the kind's rule is broken (daemon: only rosettad differs, the dist pinned; ux: only the
// dist differs, rosettad pinned; aa: one side run twice), or when a declared hash is not
// the file's. Then OUT (must not exist, outside the repository and the source) gets:
//   inputs/<side>/rosettad, inputs/<side>/dist   frozen copies, re-hashed;
//   inputs/original-dist                         the checkout's clients/app/dist, restored at the end;
//   plan.json (private: paths), declared.json    (no paths: hashes, kind, claims, floors,
//                                                options; its sha256 is the campaign id,
//                                                written before the first run);
//   runs/NN-<script>-<side>/                     one run_ui.mjs OUT per slot;
//   slots.json, ui-pair.json, verdict.txt        the slots, the gated pair document, the table.
// Each slot installs its side's dist as clients/app/dist (run_ui.mjs and the app read it
// there), runs run_ui.mjs with its side's frozen binary, and checks the run's own
// rosettad/dist hashes against the side. run_ui.mjs applies the free-disk and CPU-idle
// guards before every run (on by default for a snapshot, off for a fixture unless the
// side's options say idleGuard: true); a slot whose swap grew by more than swapRedoMb
// is run once more, the first kept as discarded. SIGINT/SIGTERM stop the running
// run_ui.mjs (it cleans up its children), restore the dist and exit 130/143.
// Exit: 0 pass (or an A/A campaign completed), 1 fail or void, 2 refused or harness error.
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, cpSync, existsSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { verifyFixture } from './fixture.mjs';
import { METRIC_IDS, SCRIPTS, writePair } from './pair-eval.mjs';
import { sha256File, treeDigest } from './run_ui.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '..', '..');
const DIST = path.join(REPO, 'clients', 'app', 'dist');
const SHA = /^[0-9a-f]{64}$/;
export const DEFAULT_OPTIONS = Object.freeze({ timeoutS: 600, idleMin: 75, guardWaitS: 1800, idleGuard: null, minFreeGb: 25, dpr: 2, swapRedoMb: 256 });
const OPTION_KEYS = Object.keys(DEFAULT_OPTIONS);
const SIDE_KEYS = ['rosettad', 'rosettadSha256', 'dist', 'distSha256', 'distFiles', 'snapshot', 'home', 'fixture', 'scripts', 'runs', 'options'];
const isOutside = (rel) => rel === '..' || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel);
const real = (p) => { try { return realpathSync(p); } catch { return path.resolve(p); } };

/** A side file's problems (empty: well-formed). Pure. */
export function sideProblems(s) {
  const p = [];
  if (!s || typeof s !== 'object') return ['not an object'];
  for (const k of Object.keys(s)) if (!SIDE_KEYS.includes(k)) p.push(`unknown key ${k}`);
  if (typeof s.rosettad !== 'string') p.push('rosettad: a path');
  if (!SHA.test(s.rosettadSha256 ?? '')) p.push('rosettadSha256: 64 hex');
  if (typeof s.dist !== 'string') p.push('dist: a path');
  if (!SHA.test(s.distSha256 ?? '')) p.push('distSha256: 64 hex');
  const oneSource = s.snapshot ? !s.home && !s.fixture : Boolean(s.home && s.fixture);
  if (!oneSource) p.push('exactly one source: snapshot, or home and fixture');
  if (!Array.isArray(s.scripts) || !s.scripts.length || s.scripts.some((x) => !SCRIPTS.includes(x)) || new Set(s.scripts).size !== s.scripts.length) {
    p.push(`scripts: a non-empty list of ${SCRIPTS.join(', ')}`);
  }
  if (!Number.isInteger(s.runs) || s.runs < 1 || s.runs > 50) p.push('runs: 1..50');
  for (const k of Object.keys(s.options ?? {})) if (!OPTION_KEYS.includes(k)) p.push(`options: unknown key ${k}`);
  return p;
}

/** The side's run options, defaults filled (idleGuard: on for a snapshot, off for a fixture). */
export function sideOptions(s) {
  const o = { ...DEFAULT_OPTIONS, ...(s.options ?? {}) };
  if (o.idleGuard === null) o.idleGuard = Boolean(s.snapshot);
  return o;
}

/**
 * What may differ between two sides, by kind (metrics:X7): daemon — rosettad only;
 * ux — the dist only; aa — nothing. Returns { kind, problems }. `kind` null infers it.
 */
export function pairProblems(a, b, kind = null) {
  const problems = [];
  const src = (s) => (s.snapshot ? `snapshot:${real(s.snapshot)}` : `fixture:${real(s.home)}|${real(s.fixture)}`);
  if (src(a) !== src(b)) problems.push('the sides read different sources');
  if (JSON.stringify(a.scripts) !== JSON.stringify(b.scripts)) problems.push('the sides run different scripts');
  if (a.runs !== b.runs) problems.push('the sides run a different number of runs');
  if (JSON.stringify(sideOptions(a)) !== JSON.stringify(sideOptions(b))) problems.push('the sides use different run options');
  const binDiffers = a.rosettadSha256 !== b.rosettadSha256;
  const distDiffers = a.distSha256 !== b.distSha256;
  const inferred = binDiffers && !distDiffers ? 'daemon' : distDiffers && !binDiffers ? 'ux' : binDiffers ? null : 'aa';
  const k = kind ?? inferred;
  if (k === 'daemon' && (distDiffers || !binDiffers)) problems.push('a daemon pair varies rosettad only: the dist must be pinned and the binaries differ');
  else if (k === 'ux' && (binDiffers || !distDiffers)) problems.push('a ux pair varies the dist only: rosettad must be pinned and the dists differ');
  else if (k === 'aa') problems.push('the sides are identical: run an A/A campaign with --aa');
  else if (k === null) problems.push('both rosettad and the dist differ: a pair varies one of them');
  else if (!['daemon', 'ux'].includes(k)) problems.push(`unknown kind ${k}`);
  return { kind: k, problems };
}

/** The slots in run order: per round, per script, both sides (abab: A then B every
 * round; abba: B first on odd rounds). Pure. */
export function schedule({ sides, scripts, runs, order = 'abab' }) {
  const out = [];
  for (let round = 0; round < runs; round += 1) {
    for (const script of scripts) {
      const pair = order === 'abba' && round % 2 === 1 ? [...sides].reverse() : sides;
      for (const side of pair) {
        const slot = out.length;
        out.push({ slot, round, script, side, dir: `${String(slot + 1).padStart(3, '0')}-${script}-${side}` });
      }
    }
  }
  return out;
}

/** Claims, declared before the campaign: [{ script, metric, maxRatio?, maxP? }]. */
export function claimProblems(claims, scripts) {
  if (!Array.isArray(claims)) return ['claims: a list'];
  const p = [];
  claims.forEach((c, i) => {
    if (!scripts.includes(c?.script)) p.push(`claim ${i}: script not in the campaign`);
    if (!METRIC_IDS.includes(c?.metric)) p.push(`claim ${i}: unknown metric`);
    if (c?.maxRatio != null && !(c.maxRatio > 0 && c.maxRatio <= 1)) p.push(`claim ${i}: maxRatio in (0, 1]`);
    if (c?.maxP != null && !(c.maxP > 0 && c.maxP < 1)) p.push(`claim ${i}: maxP in (0, 1)`);
    for (const k of Object.keys(c ?? {})) if (!['script', 'metric', 'maxRatio', 'maxP'].includes(k)) p.push(`claim ${i}: unknown key ${k}`);
  });
  return p;
}

/** The source's identity as ui-run/1 records it (pair-eval.mjs sharedIdentity). */
function sourceIdentity(s, options) {
  if (s.snapshot) {
    const meta = JSON.parse(readFileSync(path.join(s.snapshot, 'snapshot.json'), 'utf8'));
    return { walkInputsDigest: meta.walkInputsDigest, manifestSha256: meta.manifestSha256, files: meta.totals?.files, bytes: meta.totals?.bytes };
  }
  const v = verifyFixture(path.resolve(s.home), path.resolve(s.fixture));
  if (!v.ok) throw new Error(`fixture verification failed: ${v.problems.join('; ')}`);
  return { name: v.manifest.name, specSha256: v.manifest.specSha256, treeSha256: v.tree.sha256, files: v.tree.files, bytes: v.tree.bytes,
    walkInputsDigest: v.manifest.walkInputsDigest, generatorSha256: v.manifest.generatorSha256, idleGuard: options.idleGuard };
}

/** pmset's power source and Low Power Mode, best effort. */
export function powerState() {
  const batt = spawnSync('pmset', ['-g', 'batt'], { encoding: 'utf8' }).stdout || '';
  const all = spawnSync('pmset', ['-g'], { encoding: 'utf8' }).stdout || '';
  const source = /'AC Power'/.test(batt) ? 'ac' : /'Battery Power'/.test(batt) ? 'battery' : 'unknown';
  const m = all.match(/lowpowermode\s+(\d)/);
  return { source, lowPowerMode: m ? m[1] === '1' : null };
}

function copyTree(from, to) {
  // An APFS clone where possible (instant, no extra blocks), else a plain copy.
  const r = spawnSync('cp', ['-cR', from, to], { encoding: 'utf8' });
  if (r.status !== 0) { rmSync(to, { recursive: true, force: true }); cpSync(from, to, { recursive: true }); }
}

function parseSideArgs(argv) {
  const s = { scripts: ['reader-v1', 'passive-v1'], runs: 11, options: {} };
  for (let i = 0; i < argv.length; i += 1) {
    const k = argv[i];
    const v = () => argv[++i];
    if (k === '--rosettad') s.rosettad = path.resolve(v()); else if (k === '--dist') s.dist = path.resolve(v());
    else if (k === '--snapshot') s.snapshot = path.resolve(v()); else if (k === '--home') s.home = path.resolve(v());
    else if (k === '--fixture') s.fixture = path.resolve(v()); else if (k === '--scripts') s.scripts = v().split(',');
    else if (k === '--runs') s.runs = Number(v()); else if (k === '--timeout-s') s.options.timeoutS = Number(v());
    else if (k === '--idle-min') s.options.idleMin = Number(v()); else if (k === '--guard-wait-s') s.options.guardWaitS = Number(v());
    else if (k === '--idle-guard') s.options.idleGuard = true; else if (k === '--no-idle-guard') s.options.idleGuard = false;
    else if (k === '--min-free-gb') s.options.minFreeGb = Number(v()); else if (k === '--dpr') s.options.dpr = Number(v());
    else if (k === '--swap-redo-mb') s.options.swapRedoMb = Number(v());
    else throw new Error(`unknown option ${k}`);
  }
  if (!s.rosettad || !s.dist) throw new Error('side: --rosettad BIN --dist DIR and a source are required');
  if (!existsSync(path.join(s.dist, 'index.html'))) throw new Error('side: --dist has no index.html (build the renderer first)');
  s.rosettadSha256 = sha256File(s.rosettad);
  const d = treeDigest(s.dist);
  s.distSha256 = d.sha256;
  s.distFiles = d.files;
  const p = sideProblems(s);
  if (p.length) throw new Error(`side: ${p.join('; ')}`);
  return s;
}

function parseRunArgs(argv) {
  const a = { order: 'abab', dryRun: false };
  for (let i = 0; i < argv.length; i += 1) {
    const k = argv[i];
    const v = () => argv[++i];
    if (k === '--before') a.before = v(); else if (k === '--after') a.after = v(); else if (k === '--aa') a.aa = v();
    else if (k === '--out') a.out = path.resolve(v()); else if (k === '--kind') a.kind = v(); else if (k === '--claims') a.claims = v();
    else if (k === '--floors') a.floors = v(); else if (k === '--order') a.order = v(); else if (k === '--dry-run') a.dryRun = true;
    else throw new Error(`unknown option ${k}`);
  }
  const shape = a.aa ? !a.before && !a.after && !a.kind && !a.claims && !a.floors : Boolean(a.before && a.after);
  if (!shape || !a.out || !['abab', 'abba'].includes(a.order)) {
    throw new Error('usage: pair.mjs run (--before A.json --after B.json [--kind daemon|ux] [--claims C.json] [--floors AA/ui-pair.json] | --aa A.json) --out OUT [--order abab|abba] [--dry-run]');
  }
  return a;
}

const readJson = (f) => JSON.parse(readFileSync(f, 'utf8'));

/** Build the plan and the declaration; refuses (throws) on any pre-run problem. */
export function preparePair(a) {
  const named = a.aa ? { a1: readJson(a.aa), a2: readJson(a.aa) } : { before: readJson(a.before), after: readJson(a.after) };
  for (const [side, s] of Object.entries(named)) { const p = sideProblems(s); if (p.length) throw new Error(`${side}: ${p.join('; ')}`); }
  const [x, y] = Object.values(named);
  let kind = 'aa';
  if (!a.aa) {
    const r = pairProblems(x, y, a.kind ?? null);
    if (r.problems.length) throw new Error(`refused: ${r.problems.join('; ')}`);
    kind = r.kind;
  }
  const claims = a.claims ? readJson(a.claims) : [];
  const cp = claimProblems(claims, x.scripts);
  if (cp.length) throw new Error(`claims: ${cp.join('; ')}`);
  let floors = null;
  if (a.floors) {
    const aa = readJson(a.floors);
    if (aa.format !== 'ui-pair/1' || aa.kind !== 'aa' || aa.void) throw new Error('--floors must be a complete, non-void A/A ui-pair.json');
    const values = {};
    for (const r of aa.metrics) if (r.floorMs != null) values[`${r.script}:${r.metric}`] = r.floorMs;
    floors = { aaCampaignSha256: aa.campaignSha256, values };
  }
  const options = sideOptions(x);
  const source = x.snapshot ? path.resolve(x.snapshot) : path.resolve(x.home);
  const out = a.out;
  if (!isOutside(path.relative(REPO, out))) throw new Error('OUT must be outside the repository');
  if (!isOutside(path.relative(real(source), out))) throw new Error('OUT must be outside the source');
  if (existsSync(out)) throw new Error('OUT exists; refusing');
  for (const [side, s] of Object.entries(named)) {
    if (sha256File(s.rosettad) !== s.rosettadSha256) throw new Error(`${side}: rosettad does not hash to rosettadSha256`);
    if (treeDigest(s.dist).sha256 !== s.distSha256) throw new Error(`${side}: dist does not hash to distSha256`);
  }
  const declared = {
    format: 'ui-pair-declared/1', gateSet: 'a', kind, order: a.order, runsPerSide: x.runs, scripts: x.scripts, options,
    sides: Object.fromEntries(Object.entries(named).map(([side, s]) => [side, { rosettadSha256: s.rosettadSha256, distSha256: s.distSha256, distFiles: s.distFiles ?? null }])),
    source: sourceIdentity(x, options), sourceKind: x.snapshot ? 'snapshot' : 'fixture', claims, floors,
  };
  const plan = { out, sides: named, source: x.snapshot ? { snapshot: path.resolve(x.snapshot) } : { home: path.resolve(x.home), fixture: path.resolve(x.fixture) },
    slots: schedule({ sides: Object.keys(named), scripts: x.scripts, runs: x.runs, order: a.order }) };
  return { declared, plan };
}

const state = { child: null, restore: null, stopping: false };

function runSlot(plan, options, slot, outDir, log) {
  const side = plan.sides[slot.side];
  const args = [path.join(HERE, 'run_ui.mjs'), '--rosettad', path.join(plan.out, 'inputs', slot.side, 'rosettad'),
    ...(plan.source.snapshot ? ['--snapshot', plan.source.snapshot] : ['--home', plan.source.home, '--fixture', plan.source.fixture]),
    '--out', outDir, '--script', slot.script, '--timeout-s', String(options.timeoutS), '--idle-min', String(options.idleMin),
    '--guard-wait-s', String(options.guardWaitS), options.idleGuard ? '--idle-guard' : '--no-idle-guard', '--min-free-gb', String(options.minFreeGb),
    '--dpr', String(options.dpr)];
  if (!side) throw new Error('unknown side');
  return new Promise((resolve) => {
    // Its own process group: a terminal's Ctrl-C reaches this runner, which forwards one signal.
    const child = state.child = spawn(process.execPath, args, { stdio: ['ignore', 'pipe', 'pipe'], detached: true });
    const sink = (buf) => { for (const line of buf.toString().split('\n').filter(Boolean)) log(`  ${line}`); };
    child.stdout.on('data', sink);
    child.stderr.on('data', sink);
    child.on('close', (code, signal) => { state.child = null; resolve(code ?? (signal ? 128 : 2)); });
  });
}

function installDist(plan, side) {
  rmSync(DIST, { recursive: true, force: true });
  copyTree(path.join(plan.out, 'inputs', side, 'dist'), DIST);
  return treeDigest(DIST).sha256;
}

async function runPair(a) {
  const { declared, plan } = preparePair(a);
  if (a.dryRun) {
    process.stdout.write(`${JSON.stringify({ declared, slots: plan.slots.map((s) => s.dir) }, null, 1)}\n`);
    return 0;
  }
  const out = plan.out;
  mkdirSync(path.join(out, 'runs'), { recursive: true, mode: 0o700 });
  const logLines = [];
  const log = (m) => { const line = `${new Date().toISOString()} ${m}`; logLines.push(line); console.log(`ui-pair: ${m}`); };
  const flushLog = () => { try { writeFileSync(path.join(out, 'pair.log'), `${logLines.join('\n')}\n`, { mode: 0o600 }); } catch { /* best effort */ } };
  // Frozen inputs: a copy of each side's binary and dist, re-hashed (a concurrent build
  // of the source paths cannot change a side mid-campaign).
  for (const [side, s] of Object.entries(plan.sides)) {
    const dir = path.join(out, 'inputs', side);
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    cpSync(s.rosettad, path.join(dir, 'rosettad'));
    chmodSync(path.join(dir, 'rosettad'), 0o555);
    copyTree(s.dist, path.join(dir, 'dist'));
    if (sha256File(path.join(dir, 'rosettad')) !== s.rosettadSha256) throw new Error(`${side}: the copied rosettad hashes differently`);
    if (treeDigest(path.join(dir, 'dist')).sha256 !== s.distSha256) throw new Error(`${side}: the copied dist hashes differently`);
  }
  const hadDist = existsSync(DIST);
  if (hadDist) copyTree(DIST, path.join(out, 'inputs', 'original-dist'));
  state.restore = () => {
    rmSync(DIST, { recursive: true, force: true });
    if (hadDist) copyTree(path.join(out, 'inputs', 'original-dist'), DIST);
  };
  const declaredText = `${JSON.stringify(declared, null, 1)}\n`;
  writeFileSync(path.join(out, 'declared.json'), declaredText, { mode: 0o444, flag: 'wx' });
  writeFileSync(path.join(out, 'plan.json'), `${JSON.stringify(plan, null, 1)}\n`, { mode: 0o600, flag: 'wx' });
  const campaign = createHash('sha256').update(declaredText).digest('hex');
  log(`campaign ${campaign} kind ${declared.kind}, ${plan.slots.length} slots, order ${declared.order}`);
  const startedAt = new Date().toISOString();
  const done = [];
  const writeSlots = (finishedAt = null) => writeFileSync(path.join(out, 'slots.json'), `${JSON.stringify({ startedAt, finishedAt, slots: done }, null, 1)}\n`, { mode: 0o644 });
  for (const slot of plan.slots) {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      if (state.stopping) break;
      const dirName = attempt === 0 ? slot.dir : `${slot.dir}-redo`;
      const runDir = path.join(out, 'runs', dirName);
      const distSha = installDist(plan, slot.side);
      if (distSha !== declared.sides[slot.side].distSha256) throw new Error(`${slot.dir}: the installed dist hashes differently`);
      const power = powerState();
      log(`slot ${slot.dir}${attempt ? ' (swap redo)' : ''}: ${slot.script} ${slot.side} power=${power.source}${power.lowPowerMode ? ' low-power' : ''}`);
      const exitCode = await runSlot(plan, declared.options, slot, runDir, log);
      const docPath = path.join(runDir, 'ui-run.json');
      const doc = existsSync(docPath) ? readJson(docPath) : null;
      const swap = doc ? (doc.run?.guard?.swapMbAfter ?? 0) - (doc.run?.guard?.swapMbBefore ?? 0) : 0;
      const entry = { slot: slot.slot, round: slot.round, side: slot.side, script: slot.script, dir: dirName, exitCode, power, idleGuard: declared.options.idleGuard };
      log(`slot ${dirName}: exit ${exitCode}${doc ? ` outcome ${doc.run?.outcome} I1 ${doc.metrics?.I1?.pass} swap +${swap.toFixed(0)} MB` : ' (no ui-run.json)'}`);
      if (doc && attempt === 0 && swap > declared.options.swapRedoMb) { done.push({ ...entry, discarded: true }); writeSlots(); continue; }
      done.push(entry);
      writeSlots();
      break;
    }
    if (state.stopping) break;
  }
  state.restore();
  state.restore = null;
  writeSlots(new Date().toISOString());
  const forbidden = [out, REPO, os.homedir(), os.userInfo().username, os.hostname(), ...Object.values(plan.source),
    ...Object.values(plan.sides).flatMap((s) => [s.rosettad, s.dist])];
  const { pair, table, ok, errors } = writePair(out, { forbidden });
  process.stdout.write(table);
  if (!ok) { for (const e of errors.slice(0, 20)) log(`privacy: ${e}`); log('ui-pair.json not written (privacy gate)'); flushLog(); return 2; }
  log(`verdict ${pair.summary.verdict}`);
  flushLog();
  if (state.stopping) return state.stopping;
  return pair.summary.verdict === 'pass' || pair.summary.verdict === 'aa' ? 0 : 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [cmd, ...rest] = process.argv.slice(2);
  const stop = (code, sig) => {
    if (state.stopping) return;
    state.stopping = code;
    console.error(`ui-pair: ${sig}: stopping after the current run`);
    if (state.child) { try { process.kill(state.child.pid, sig); } catch { /* gone */ } }
  };
  process.on('SIGINT', () => stop(130, 'SIGINT'));
  process.on('SIGTERM', () => stop(143, 'SIGTERM'));
  const main = async () => {
    if (cmd === 'side') { process.stdout.write(`${JSON.stringify(parseSideArgs(rest), null, 1)}\n`); return 0; }
    if (cmd === 'run') return runPair(parseRunArgs(rest));
    throw new Error('usage: pair.mjs side … | pair.mjs run …');
  };
  main().then((code) => process.exit(code), (error) => {
    try { state.restore?.(); } catch (e) { console.error(`ui-pair: restoring clients/app/dist failed: ${e.message}`); }
    console.error(`ui-pair: ${error.message}`);
    process.exit(2);
  });
}
