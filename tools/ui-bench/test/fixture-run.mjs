#!/usr/bin/env node
// The real-daemon, real-app fixture test (design metrics:X10): the real release
// rosettad, SDK, hooks and renderer over a home anyone can regenerate, asserting
// invariants and digests only, never a timing threshold.
//
// What it covers, honestly: since ux:X7 the app's columns are up about 0.15 s after
// consent, well before the import ends, and reader-v1 selects NEWEST's project while
// NEWEST is still Pending, so NEWEST is drawn not imported and flips in place (once its
// first aim's miss is aimed again; see fixture-checks.mjs). It tests flip DELIVERY
// (every root's flip reaches the UI, in the daemon's order, after the daemon made it),
// the end state, selection and list invariants, privacy and the digests; Pending paint
// it reaches, but timing decides whether, so it asserts nothing from it (I1 c1's breach
// class, H5), and c4, c7 and the opened-while-Pending face stay unreached. Each run's
// regime is recorded and printed (fixture-checks.mjs regimeOf), and a run in another
// regime is flagged.
//
// LOCAL-ONLY: it needs a release rosettad, the built renderer (clients/app/dist),
// Electron with a display and macOS (the bench's guards read `top`/`sysctl`/`sw_vers`,
// its digests use /usr/bin/sqlite3), and nothing in CI builds rosettad for an Electron
// job. It is not *.test.mjs, so the unit suite never runs it.
//
//   npm run test:ui-readiness        (builds the dist, then release rosettad and
//                                     bench-corpus with --locked, and runs this)
//   node tools/ui-bench/test/fixture-run.mjs [--rosettad BIN --bench-corpus BIN]
//        [--work DIR] [--reader-only] [--expected FILE] [--write-expected] [--keep]
//        [--timeout-s 600]
//
// Without --rosettad/--bench-corpus it runs `cargo build --release --locked -p rosettad
// -p rosetta-bench-corpus` and takes the exact executables cargo reports (never an
// mtime-picked artifact). Each is hashed right after cargo returns, copied into the
// work directory and hashed again (a mismatch refuses), so a later rebuild of a shared
// target directory cannot swap it mid-run.
//
// Preconditions (exit 2): /tmp/bench does not exist and no `.git` sits at /tmp or /
// (bench-corpus writes every cwd as /tmp/bench/project-N, and the meta resolver looks
// for a checkout at each cwd and its ancestors, so one there would change the projects
// and every digest); WORK lies outside the repository and the home directory.
//
// Steps, per script (reader-v1, then passive-v1, a few seconds more; --reader-only skips
// it), over one fixture home (fixture.mjs) generated under WORK (a fresh temp directory
// unless --work):
//   1. run_ui.mjs --home, with --keep so the daemon's stdout can name the flips of
//      NEWEST, of each @newest horizon and of the last root;
//   2. the fixture-vocabulary ranked digest (fixture-search.mjs, a warm start on the
//      run's own db: the R1 workload finds nothing in a synthetic corpus);
//   3. delete what run_ui would have deleted (the db only once no daemon can hold it),
//      and check the home was not written;
//   4. judge ui-run.json with fixture-checks.mjs against fixture-expected.json.
// Then the home, binaries, dbs and profiles go (unless --keep); WORK keeps each run's
// ui-run.json, digests/ and run.log, and report.json. run_ui's exit code is not the
// verdict: reader-v1's aim can miss at the end-of-import reorder (ux:X9), a product
// finding H7 records; the test reports it and reads ui-run.json instead.
// --write-expected rewrites the expected file from this run, only when every other
// check passed (README: when that is legitimate). Exit 0 all checks passed, 1 a check
// failed, 2 a setup error (a precondition, a missing binary, a build failure, no
// renderer build), 130/143/129 after SIGINT/SIGTERM/SIGHUP (run_ui, in its own process
// group, gets one SIGINT or SIGTERM and stops its children; the home, dbs and profiles
// are deleted as on a normal end).
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, chmodSync, existsSync, accessSync, constants, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, statSync,
  writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { flipStamps, parseDaemonLog } from '../daemon-lines.mjs';
import { buildFixture, removeFixture, verifyFixture } from '../fixture.mjs';
import { DOCUMENTED_REGIME, checkRun, digestsOf, sameHomeDigestsOf } from './fixture-checks.mjs';
import { fixtureSearch } from './fixture-search.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '..', '..', '..');
const RUN_UI = path.join(HERE, '..', 'run_ui.mjs');
const EXPECTED = path.join(HERE, 'fixture-expected.json');
const say = (msg) => console.log(`ui-fixture: ${msg}`);
const sha256 = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');

class SetupError extends Error {}

function parseArgs(argv) {
  const a = { passive: true, keep: false, writeExpected: false, expected: EXPECTED, timeoutS: 600 };
  for (let i = 0; i < argv.length; i += 1) {
    const k = argv[i];
    const v = () => argv[++i];
    if (k === '--rosettad') a.rosettad = v(); else if (k === '--bench-corpus') a.benchCorpus = v(); else if (k === '--work') a.work = v();
    else if (k === '--reader-only') a.passive = false; else if (k === '--expected') a.expected = path.resolve(v());
    else if (k === '--write-expected') a.writeExpected = true; else if (k === '--keep') a.keep = true; else if (k === '--timeout-s') a.timeoutS = Number(v());
    else throw new SetupError(`unknown option ${k}`);
  }
  if (Boolean(a.rosettad) !== Boolean(a.benchCorpus)) throw new SetupError('pass both --rosettad and --bench-corpus, or neither (then cargo builds them)');
  return a;
}

/** An executable regular file, or a SetupError that names what is missing. */
function executable(file, what) {
  const abs = path.resolve(file);
  try { if (!statSync(abs).isFile()) throw new Error(); accessSync(abs, constants.X_OK); } catch {
    throw new SetupError(`the ${what} binary is missing or not executable: ${abs}`);
  }
  return abs;
}

/**
 * The machine assumption every pinned digest rests on: bench-corpus writes each cwd as
 * /tmp/bench/project-N, and the meta resolver looks for a `.git` at the cwd and at each
 * ancestor (crates/pipeline/src/project.rs find_git_dir); a checkout there would turn
 * the four per-cwd projects into others. Nothing at /tmp/bench (not even a link) and no
 * `.git` at /tmp or / keeps every ancestor of every such cwd clear.
 */
function checkMachine() {
  let bench = false;
  try { lstatSync('/tmp/bench'); bench = true; } catch { /* absent: as required */ }
  if (bench) throw new SetupError('/tmp/bench exists: the fixture\'s cwds are /tmp/bench/project-N, and a checkout there would change its projects and every digest; move it away');
  for (const d of ['/tmp', '/']) {
    let git = false;
    try { lstatSync(path.join(d, '.git')); git = true; } catch { /* absent */ }
    if (git) throw new SetupError(`${path.join(d, '.git')} exists: the meta resolver would take it as the checkout of every fixture cwd`);
  }
}

const real = (p) => { try { return realpathSync(p); } catch { return path.resolve(p); } };
const within = (child, parent) => { const rel = path.relative(parent, child); return rel === '' || (rel !== '..' && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel)); };
/** WORK holds the fixture home, which fixture.mjs refuses inside the repository or the home directory. */
function checkWork(dir, what) {
  const d = real(dir);
  for (const [name, p] of [['the repository', REPO], ['the home directory', os.homedir()]]) {
    const q = real(p);
    if (within(d, q) || within(q, d)) throw new SetupError(`${what} must lie outside ${name} (pass --work DIR elsewhere)`);
  }
}

/** cargo's own report of the release executables it built (exact paths), each hashed at once. */
function cargoBuild() {
  say('cargo build --release --locked -p rosettad -p rosetta-bench-corpus');
  const r = spawnSync('cargo', ['build', '--release', '--locked', '-p', 'rosettad', '-p', 'rosetta-bench-corpus', '--message-format=json-render-diagnostics'],
    { cwd: REPO, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'], maxBuffer: 1 << 28 });
  if (r.error || r.status !== 0) throw new SetupError(`cargo build failed (${r.error?.message ?? `exit ${r.status ?? r.signal}`})`);
  const exe = {};
  for (const line of r.stdout.split('\n')) {
    if (!line.startsWith('{')) continue;
    const msg = JSON.parse(line);
    if (msg.reason === 'compiler-artifact' && msg.executable && ['rosettad', 'bench-corpus'].includes(msg.target?.name)) exe[msg.target.name] = msg.executable;
  }
  if (!exe.rosettad || !exe['bench-corpus']) throw new SetupError('cargo reported no rosettad or bench-corpus executable');
  const rosettad = executable(exe.rosettad, 'rosettad');
  const benchCorpus = executable(exe['bench-corpus'], 'bench-corpus');
  return { rosettad: { path: rosettad, sha256: sha256(rosettad) }, benchCorpus: { path: benchCorpus, sha256: sha256(benchCorpus) } };
}

// What a signal must stop or clean up. run_ui and the warm daemon run in their own
// process groups, so a terminal's Ctrl-C reaches only this process, which forwards
// exactly one signal to each.
const state = { child: null, warm: null, stopping: null, cleanup: null };
const SIGNALS = { SIGINT: { code: 130, forward: 'SIGINT' }, SIGTERM: { code: 143, forward: 'SIGTERM' }, SIGHUP: { code: 129, forward: 'SIGINT' } };
function onSignal(sig) {
  if (state.stopping) { say(`${sig} again: still stopping`); return; }
  state.stopping = sig;
  say(`${sig}: stopping`);
  const alive = (c) => c && c.exitCode === null && c.signalCode === null;
  if (alive(state.child)) state.child.kill(SIGNALS[sig].forward);
  if (alive(state.warm)) state.warm.kill('SIGINT');
  // Nothing running: clean up now. Otherwise main() ends once the child has exited.
  if (!alive(state.child) && !alive(state.warm)) {
    try { state.cleanup?.(); } catch (error) { console.error(`ui-fixture: cleanup: ${error.message}`); }
    process.exit(SIGNALS[sig].code);
  }
}

/** run_ui.mjs over the fixture; resolves with its exit code (the verdict comes from ui-run.json). */
function runUi(args) {
  return new Promise((resolve) => {
    const env = { ...process.env };
    delete env.ROSETTA_ARTIFACT_ROOTS; // the fixture's projection must not depend on the caller's environment
    const c = state.child = spawn(process.execPath, [RUN_UI, ...args], { stdio: 'inherit', env, detached: true });
    c.on('error', (error) => { say(`run_ui: ${error.message}`); });
    c.on('exit', (code, signal) => { state.child = null; resolve(code ?? (signal ? 128 : 2)); });
  });
}

/**
 * The daemon's flips on the import clock (daemon seconds after `consent-granted`), from
 * the tree-admission events (daemon-lines.mjs flipStamps): the batches that printed
 * `newest-giant-ready` (NEWEST is the fixture's newest giant) and the two @newest
 * horizons, and the last batch that flipped a root. The UI's receipts are ordered after
 * each batch's COMMIT stamp (`elapsedS`), which precedes every byte the hub sends of it;
 * its `publishedS` is stamped after the send returns, so a receipt may precede it (by
 * 1.3 ms in one run under load). The last root's `publishedS` is kept for the daemon's
 * own order against `runtime-readable`. H-all is checked against that batch, not
 * against `runtime-readable`: that milestone is stamped at the runtime's close, after
 * its last tree flip, and has no unrounded JSON twin (10 ms steps), so the UI can
 * receive the last flip before it (README, "Fixture test").
 */
function daemonFlips(stdoutLog, doc) {
  const none = { ms: null, h24Ms: null, h7dMs: null, lastRootMs: null, lastRootPublishedMs: null };
  if (!existsSync(stdoutLog)) return { ...none, why: 'no daemon stdout' };
  const d = parseDaemonLog(readFileSync(stdoutLog, 'utf8'));
  const ng = d.milestones['newest-giant-ready'];
  const consent = d.milestones['consent-granted'];
  if (!ng?.precise || !consent?.precise) return { ...none, why: 'no unrounded newest-giant-ready / consent-granted' };
  const imp = (s) => (s === null || s === undefined ? null : Math.round((s - consent.elapsedS) * 1e4) / 10);
  const f = flipStamps(d);
  const missing = [['NEWEST', f.newest, 'newest-giant-ready'], ['24h@newest', f.h24, 'recent-ready:24h@newest'], ['7d@newest', f.h7d, 'recent-ready:7d@newest'],
    ['last root', f.lastRoot, null]]
    .filter(([, x]) => x === null || x.commitS === null)
    .map(([name, x, key]) => (x === null ? (key ? `no tree-admission batch before ${key}` : 'no tree-admission batch that flipped a root') : `${name}'s batch has no commit stamp`));
  return { ms: imp(f.newest?.commitS), h24Ms: imp(f.h24?.commitS), h7dMs: imp(f.h7d?.commitS), lastRootMs: imp(f.lastRoot?.commitS),
    lastRootPublishedMs: imp(f.lastRoot?.publishedS), publishUs: f.newest?.commitS == null ? null : Math.round((f.newest.publishedS - f.newest.commitS) * 1e6),
    milestoneMs: imp(ng.elapsedS), gapUs: f.newest === null ? null : Math.round((ng.elapsedS - f.newest.publishedS) * 1e6),
    docMilestoneMs: doc?.daemon?.milestonesMs?.['newest-giant-ready'] ?? null, batches: d.treeAdmissions.length, why: missing.join('; ') || null };
}

/** Why a daemon may still hold the run's db (then it is kept), or null. */
function dbHeldBy(out, doc) {
  if ((doc?.run?.outcomeReasons || []).includes('daemon-did-not-stop')) return 'run_ui saw its daemon not stop';
  let files = [];
  try { files = readdirSync(path.join(out, 'announce')).filter((f) => /^[0-9]+\.json$/.test(f)); } catch { return null; }
  // A daemon removes its announcement as it exits; a live pid behind one left behind
  // may be that daemon (or a reuse: keeping the db then costs only disk).
  for (const f of files) {
    const pid = Number(f.slice(0, -'.json'.length));
    try { process.kill(pid, 0); return `the run's daemon (announced pid ${pid}) may still be running`; } catch (error) { if (error.code === 'EPERM') return `announced pid ${pid} is alive`; }
  }
  return null;
}

/** What run_ui deletes without --keep (the test kept it to read the daemon's stdout); the db only once no daemon can hold it. */
function dropRunBulk(out, keepAll, doc) {
  if (keepAll || !existsSync(out)) return null;
  const held = dbHeldBy(out, doc);
  const doomed = ['user-data', 'stdout.log', 'stderr.log', 'electron.log', 'digest-errors.log', 'privacy-errors.txt', 'ui-run.rejected.json'];
  // A kept db keeps its announcement too, so the final cleanup can look again.
  if (!held) doomed.push('db', 'announce');
  for (const p of doomed) rmSync(path.join(out, p), { recursive: true, force: true });
  if (held) say(`kept ${path.join(out, 'db')}: ${held}; delete it once that daemon is gone`);
  return held;
}

const fmt = (ok) => (ok === true ? 'PASS' : ok === null ? 'n/a ' : 'FAIL');
const readDoc = (file) => { try { return JSON.parse(readFileSync(file, 'utf8')); } catch { return null; } };

async function main() {
  const a = parseArgs(process.argv.slice(2));
  for (const sig of Object.keys(SIGNALS)) process.on(sig, () => onSignal(sig));
  checkMachine();
  if (a.work) checkWork(a.work, '--work');
  else checkWork(os.tmpdir(), `the temp directory (${os.tmpdir()})`);
  if (!existsSync(path.join(REPO, 'clients', 'app', 'dist', 'index.html'))) throw new SetupError('no renderer build: run `npm run build` first');
  const built = a.rosettad
    ? { rosettad: { path: executable(a.rosettad, 'rosettad') }, benchCorpus: { path: executable(a.benchCorpus, 'bench-corpus') } }
    : cargoBuild();
  for (const b of Object.values(built)) if (!b.sha256) b.sha256 = sha256(b.path);
  if (state.stopping) return SIGNALS[state.stopping].code;
  const expected = existsSync(a.expected) ? JSON.parse(readFileSync(a.expected, 'utf8')) : null;

  let work;
  if (a.work) { work = path.resolve(a.work); if (existsSync(work)) throw new SetupError('--work exists; refusing'); mkdirSync(work, { recursive: true, mode: 0o700 }); }
  else work = mkdtempSync(path.join(os.tmpdir(), 'rosetta-ui-fixture-'));
  say(`work directory ${work}`);
  const fixtureDir = path.join(work, 'fixture');
  const runDirs = [];
  state.cleanup = () => {
    for (const out of runDirs) dropRunBulk(out, a.keep, readDoc(path.join(out, 'ui-run.json')));
    if (!a.keep) { removeFixture(fixtureDir); rmSync(path.join(work, 'bin'), { recursive: true, force: true }); }
  };

  try {
    // Private copies of the exact binaries, the same bytes cargo just reported.
    mkdirSync(path.join(work, 'bin'), { mode: 0o700 });
    const bin = {};
    for (const [k, b] of Object.entries(built)) {
      bin[k] = path.join(work, 'bin', path.basename(b.path));
      copyFileSync(b.path, bin[k]);
      chmodSync(bin[k], 0o755);
      const copied = sha256(bin[k]);
      if (copied !== b.sha256) throw new SetupError(`${b.path} changed between ${a.rosettad ? 'its hash' : 'the build'} and its copy (a concurrent build of a shared target directory?); run again`);
    }
    say(`binaries rosettad ${built.rosettad.sha256.slice(0, 12)}, bench-corpus ${built.benchCorpus.sha256.slice(0, 12)} (copied, hashes equal)`);
    const t0 = performance.now();
    const fx = buildFixture({ benchCorpus: bin.benchCorpus, dir: fixtureDir });
    say(`fixture ${fx.manifest.name}: ${fx.manifest.tree.files} files, ${(fx.manifest.tree.bytes / 1e6).toFixed(1)} MB, tree ${fx.manifest.tree.sha256.slice(0, 12)}, `
      + `${fx.manifest.shape.roots} roots (24h@newest ${fx.manifest.shape.h24}, 7d@newest ${fx.manifest.shape.h7d}), generated in ${Math.round(performance.now() - t0)} ms`);

    const results = [];
    for (const script of a.passive ? ['reader-v1', 'passive-v1'] : ['reader-v1']) {
      if (state.stopping) break;
      const out = path.join(work, `run-${script}`);
      runDirs.push(out);
      let doc = null;
      let code = null;
      let flip = null;
      let search = { error: 'not run' };
      try {
        code = await runUi(['--rosettad', bin.rosettad, '--home', fx.home, '--fixture', fx.manifestPath, '--out', out, '--script', script,
          '--keep', '--timeout-s', String(a.timeoutS)]);
        doc = readDoc(path.join(out, 'ui-run.json'));
        flip = daemonFlips(path.join(out, 'stdout.log'), doc);
        const db = path.join(out, 'db', 'rosetta.sqlite');
        const held = dbHeldBy(out, doc);
        if (!doc || state.stopping) search = { error: state.stopping ? 'stopping' : 'no run document' };
        else if (held) search = { error: `not run: ${held}` };
        else if (!existsSync(db)) search = { error: 'no run db' };
        else search = await fixtureSearch({ bin: bin.rosettad, db, home: fx.home, track: (c) => { state.warm = c; } });
      } finally {
        state.warm = null;
        dropRunBulk(out, a.keep, doc);
      }
      if (state.stopping) break;
      const homeAfter = verifyFixture(fx.home, fx.manifestPath);
      let checks;
      let recorded = null;
      if (!doc) {
        const tail = existsSync(path.join(out, 'run.log')) ? readFileSync(path.join(out, 'run.log'), 'utf8').trim().split('\n').slice(-8).join('\n  ') : 'no run.log';
        checks = [{ id: 'document', ok: false, detail: `run_ui exited ${code} and wrote no ui-run.json (its reason is the \`ui-bench:\` line above); run.log ends:\n  ${tail}` }];
      } else {
        ({ checks, recorded } = checkRun(doc, { script, manifest: fx.manifest, expected, search,
          flips: { newestMs: flip.ms, h24Ms: flip.h24Ms, h7dMs: flip.h7dMs, lastRootMs: flip.lastRootMs, lastRootPublishedMs: flip.lastRootPublishedMs } }));
        checks.push({ id: 'daemon-flips-found', ok: flip.why === null,
          detail: flip.why ?? `NEWEST's flip committed at ${flip.ms} ms (published ${flip.publishUs} µs later, ${flip.gapUs} µs before newest-giant-ready), `
            + `24h@newest ${flip.h24Ms} ms, 7d@newest ${flip.h7dMs} ms, last root ${flip.lastRootMs} ms (published ${flip.lastRootPublishedMs} ms), ${flip.batches} batches` });
        checks.push({ id: 'daemon-stdout-is-this-run', ok: flip.docMilestoneMs !== null && flip.milestoneMs !== undefined && Math.abs(flip.milestoneMs - flip.docMilestoneMs) <= 0.2,
          detail: `newest-giant-ready ${flip.milestoneMs} ms from the kept stdout, ${flip.docMilestoneMs} ms in the document` });
      }
      checks.push({ id: 'home-unchanged', ok: homeAfter.ok, detail: homeAfter.ok ? 'tree digest unchanged after the run' : homeAfter.problems.join('; ') });
      results.push({ script, exit: code, doc, checks, recorded, flip, search });
      say(`${script}: run_ui exit ${code} (not the verdict)`);
      for (const c of checks) if (c.ok !== true) say(`  ${fmt(c.ok)} ${c.id}: ${c.detail}`);
      say(`  ${checks.filter((c) => c.ok === true).length} passed, ${checks.filter((c) => c.ok === false).length} failed, ${checks.filter((c) => c.ok === null).length} n/a`);
      if (recorded) {
        const inv = recorded.h7Invariant;
        const g = recorded.regime;
        say(`  regime ${g.name}: canonical-ready ${g.canonicalReadyMs} ms, H1 ${g.h1Ms} ms, NEWEST first drawn ${g.newestFirstDrawStanding}, `
          + `not-imported rows drawn ${g.notImportedDrawnMax}, ready opens before canonical ${g.readyOpensBeforeCanonical}, pending-face opens ${g.pendingFaceOpens}, H5 n ${g.h5n}`);
        if (g.name !== DOCUMENTED_REGIME[script]) {
          say(`  NOTE: not the documented regime (${DOCUMENTED_REGIME[script]}, README "Fixture test"): timing moved NEWEST's flip against the `
            + 'reader\'s selection (or the import against the first paint); nothing is asserted from the regime, so this fails nothing');
        }
        say(`  recorded: H7 invariant ${inv?.pass ? 'pass' : 'FAIL'} (aims ${inv?.aims}, misses ${inv?.aimMisses}, selection jumps ${inv?.selectionJumps}); `
          + `I1 exercised ${JSON.stringify(recorded.i1Exercised)}; H4 n ${recorded.h4.n}, pending face ${recorded.h4.pendingFace}`);
      }
    }

    // The digests must also agree between the scripts of this one test run, which read
    // one generated home (so strict_noid's file evidence must agree too).
    if (results.length > 1 && results.every((r) => r.doc)) {
      const [x, y] = results.map((r) => ({ ...digestsOf(r.doc), ...sameHomeDigestsOf(r.doc) }));
      const keys = ['logicalTotal', 'strictNoidTotal', 'strictNoidRTotal', 'sourceSetDigest', 'rankedIdsSha256', 'finalRoots', 'finalReadySessions'];
      const ok = keys.every((k) => x[k] === y[k]) && JSON.stringify(x.counts) === JSON.stringify(y.counts)
        && results[0].search?.rankedIdsSha256 === results[1].search?.rankedIdsSha256;
      results[0].checks.push({ id: 'scripts-agree', ok, detail: ok ? 'reader-v1 and passive-v1 digests equal' : 'reader-v1 and passive-v1 digests differ' });
      if (!ok) say('  FAIL scripts-agree: reader-v1 and passive-v1 digests differ');
    }

    const all = results.flatMap((r) => r.checks);
    const failed = all.filter((c) => c.ok === false);
    if (a.writeExpected && !state.stopping) {
      const reader = results.find((r) => r.script === 'reader-v1');
      const blocking = all.filter((c) => c.ok === false && !c.id.startsWith('expected'));
      if (!reader?.doc || blocking.length) throw new Error(`refusing to write expected digests: ${blocking.length} non-digest check(s) failed`);
      const next = { format: 'ui-fixture-expected/1', fixture: { name: fx.manifest.name, specSha256: fx.manifest.specSha256, treeSha256: fx.manifest.tree.sha256,
        files: fx.manifest.tree.files, bytes: fx.manifest.tree.bytes }, equality: digestsOf(reader.doc),
        search: { rankedIdsSha256: reader.search.rankedIdsSha256, perCase: reader.search.perCase } };
      writeFileSync(a.expected, `${JSON.stringify(next, null, 1)}\n`);
      say(`wrote ${path.relative(REPO, a.expected)}${expected ? '' : ' (new)'}; review its diff before committing it`);
    }
    const report = { binaries: { rosettadSha256: built.rosettad.sha256, benchCorpusSha256: built.benchCorpus.sha256 },
      runs: results.map((r) => ({ script: r.script, exit: r.exit, flip: r.flip, search: r.search, recorded: r.recorded, digests: r.doc ? digestsOf(r.doc) : null,
        checks: r.checks, metrics: r.doc ? summary(r.doc) : null })) };
    writeFileSync(path.join(work, 'report.json'), `${JSON.stringify(report, null, 1)}\n`);
    say(`report ${path.join(work, 'report.json')}`);
    if (state.stopping) { say(`interrupted (${state.stopping}): no verdict`); return SIGNALS[state.stopping].code; }
    say(failed.length || !results.length ? `FAILED: ${failed.length} check(s)` : `ok: ${all.filter((c) => c.ok === true).length} checks passed`);
    return failed.length || !results.length ? 1 : 0;
  } finally {
    state.cleanup();
  }
}

/** The headline values, for the report (never asserted). */
function summary(doc) {
  const m = doc.metrics || {};
  return { H1: m.H1?.ms, H1metaPush: m.H1?.metaPushMs, H1statusPoll: m.H1?.statusPollMs, H2: m.H2?.ms, H2receipt: m.H2?.receiptMs,
    H2firstDrawStanding: m.H2?.firstDrawStanding, H2flippedBeforeDrawn: m.H2?.flippedBeforeDrawn,
    H3h24: m.H3?.h24, H3h7d: m.H3?.h7d, Hall: m.Hall, H4: { n: m.H4?.n, p50Ms: m.H4?.p50Ms, maxMs: m.H4?.maxMs, byPhase: m.H4?.byPhase, pendingFace: m.H4?.pendingFace },
    H5: m.H5, H7: m.H7?.invariant, H8: { searchLagMs: m.H8?.searchLagMs, rows: m.H8?.rows, hitDigest: m.H8?.hitDigest, chartsAllMs: m.H8?.chartsAllMs, portfolioMs: m.H8?.portfolioMs },
    D2giant: m.D2?.newestGiant, D2notImportedSelected: m.D2?.notImportedSelected, I1: m.I1?.checks, clocks: doc.clocks, daemon: doc.daemon?.milestonesMs,
    treeAdmission: doc.daemon?.treeAdmission, memoryPeak: doc.daemon?.memoryPeak, privacy: doc.privacy, wallS: doc.run?.wallS };
}

main().then((code) => process.exit(code), (error) => {
  try { state.cleanup?.(); } catch { /* reported below */ }
  console.error(`ui-fixture: ${error instanceof SetupError ? 'setup' : 'error'}: ${error.message}`);
  process.exit(error instanceof SetupError ? 2 : 1);
});
