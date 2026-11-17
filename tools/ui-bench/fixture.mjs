// The ui-bench fixture home: a small deterministic synthetic home that anyone can
// regenerate, the input of tools/ui-bench/test/fixture-run.mjs (run_ui.mjs --home).
//
// Agent formats stay in crates/bench-corpus (iron rules 1 and 6): every transcript is
// written by `bench-corpus gen <agent> --sessions N --lines M --seed S --out DIR`, never
// by this file. What bench-corpus does not do, this file does, and nothing else:
//   1. it places each corpus under its agent's data root in one home (claude-code under
//      .claude, codex under .codex); bench-corpus derives every cwd from
//      /tmp/bench/project-N, so the two agents share projects by construction;
//   2. it adds a newest giant: a second claude-code corpus of one session just over the
//      daemon's 64 MiB big-tree line (LARGE_BYTES), so the big lane and the
//      `newest-giant-ready` milestone are exercised;
//   3. it sets every file's mtime from a fixed base, by sorted relative path, so the walk
//      order and the @newest horizons are fixed: the giant is NEWEST (GIANT_AGE_H hours
//      before the base), and the other files take AGES_H in a fixed stride over their
//      sorted paths, so both agents have members in 24h@newest and in 7d@newest. The
//      base lies after every timestamp bench-corpus writes (2026-01-01 + 180 days, plus
//      a session's own span), so no session starts after it was last modified;
//   4. it makes the tree read-only (files 0444, directories 0555), as the frozen R1
//      snapshot is, and writes a manifest beside the home (never inside it): the spec,
//      the generator's sha256 and a tree digest of the generated files.
// The claude-code adapter takes a session's updated_at from its file's mtime, and the
// codex rollouts here carry no state database, so mtimes alone order the sessions.
// bench-corpus writes no subagent transcripts, so every root tree is one session.
//
// run_ui.mjs verifies a fixture home against its manifest before every run
// (verifyFixture), as it verifies the R1 snapshot with the R1 kit; first it refuses a
// home outside a fixture's place (placeProblems: never the user's home or the repo).
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const LIB = createRequire(path.join(HERE, '..', 'r1-bench', 'package.json'))('./r1_snapshot_lib.js');
const HOUR_MS = 3_600_000;
const DAY_H = 24;
/** The daemon's big-tree line (rosettad ingest_metrics LARGE_BYTES). */
export const LARGE_BYTES = 64 * 1024 * 1024;

/** The one fixture. Changing any value is a new fixture: the expected digests change. */
export const FIXTURE_SPEC = Object.freeze({
  name: 'ui-fixture-v1',
  corpora: [
    { agent: 'claude-code', dataRoot: '.claude', sessions: 16, lines: 4000, seed: 41, toolRatio: 0.3, giant: false },
    { agent: 'claude-code', dataRoot: '.claude', sessions: 1, lines: 36000, seed: 42, toolRatio: 0.3, giant: true },
    { agent: 'codex', dataRoot: '.codex', sessions: 9, lines: 1500, seed: 43, toolRatio: 0.3, giant: false },
  ],
  baseEpochMs: Date.UTC(2026, 8, 1), // 2026-09-01T00:00:00Z
  giantAgeH: 3,
  // One age (hours before the base) per non-giant file. Relative to NEWEST (3 h):
  // 4 within 24 h, 7 more within 7 d, 14 older (9 to 60 days).
  agesH: [5, 8, 13, 21, 30, 40, 55, 75, 100, 130, 160,
    ...[9, 11, 14, 17, 20, 24, 28, 32, 36, 40, 45, 50, 55, 60].map((d) => d * DAY_H)],
  stride: 7, // coprime with agesH.length, so the ages interleave the sorted paths
});

const byteOrder = (l, r) => LIB.byteCompare(l, r);
export const specSha256 = (spec = FIXTURE_SPEC) => createHash('sha256').update(JSON.stringify(spec)).digest('hex');
const sha256File = (file) => LIB.sha256File(file);

/** Every regular file under `home`, by home-relative path (`/`), in UTF-8 byte order. */
function filesOf(home) {
  return LIB.filesBelow(home).sort((l, r) => byteOrder(l.rel, r.rel));
}

/** sha256 over "rel\tsize\tmtime_ms\tsha256(file)\n" of every file under home, in path order. */
export function treeDigest(home) {
  const hash = createHash('sha256');
  let bytes = 0;
  const files = filesOf(home);
  for (const f of files) {
    const st = fs.statSync(f.abs, { bigint: true });
    bytes += Number(st.size);
    hash.update(`${f.rel}\t${st.size}\t${st.mtimeNs / 1_000_000n}\t${sha256File(f.abs)}\n`);
  }
  return { sha256: hash.digest('hex'), files: files.length, bytes };
}

/** The roots each @newest horizon holds, NEWEST included, from the spec alone. */
export function horizonCounts(spec = FIXTURE_SPEC) {
  const within = (span) => 1 + spec.agesH.filter((age) => age - spec.giantAgeH <= span).length;
  return { h24: within(24), h7d: within(7 * 24) };
}

function run(cmd, args) {
  const r = spawnSync(cmd, args, { encoding: 'utf8', maxBuffer: 1 << 24 });
  if (r.error) throw new Error(`${path.basename(cmd)}: ${r.error.message}`);
  if (r.status !== 0) throw new Error(`${path.basename(cmd)} ${args[0]} ${args[1]} exited ${r.status}: ${(r.stderr || '').trim().slice(-300)}`);
  return r.stdout;
}

function chmodTree(dir, fileMode, dirMode) {
  for (const d of LIB.directoriesBelow(dir).reverse()) {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) if (e.isFile()) fs.chmodSync(path.join(d, e.name), fileMode);
    fs.chmodSync(d, dirMode);
  }
}

/** Remove a fixture directory, read-only home included. */
export function removeFixture(dir) {
  if (!fs.existsSync(dir)) return;
  for (const d of LIB.directoriesBelow(dir)) fs.chmodSync(d, 0o700);
  fs.rmSync(dir, { recursive: true, force: true });
}

/**
 * Generate the fixture into `dir` (must not exist): `dir/home` and `dir/fixture.json`.
 * Returns { home, manifestPath, manifest }.
 */
export function buildFixture({ benchCorpus, dir, spec = FIXTURE_SPEC }) {
  if (fs.existsSync(dir)) throw new Error('fixture directory exists; refusing');
  if (spec.agesH.length < 2 || gcd(spec.stride, spec.agesH.length) !== 1) throw new Error('fixture spec: stride must be coprime with agesH.length');
  const home = path.join(dir, 'home');
  const stage = path.join(dir, 'stage');
  fs.mkdirSync(home, { recursive: true, mode: 0o700 });
  const giants = new Set();
  spec.corpora.forEach((c, i) => {
    const out = path.join(stage, String(i));
    run(benchCorpus, ['gen', c.agent, '--sessions', String(c.sessions), '--lines', String(c.lines), '--seed', String(c.seed),
      '--tool-ratio', String(c.toolRatio), '--out', out, '--json']);
    for (const f of LIB.filesBelow(out)) {
      const rel = `${c.dataRoot}/${f.rel}`;
      const target = path.join(home, rel);
      if (fs.existsSync(target)) throw new Error('fixture spec: two corpora wrote the same file');
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.renameSync(f.abs, target);
      if (c.giant) giants.add(rel);
    }
  });
  fs.rmSync(stage, { recursive: true, force: true });

  const files = filesOf(home);
  const giant = files.filter((f) => giants.has(f.rel));
  if (giant.length !== 1 || fs.statSync(giant[0].abs).size <= LARGE_BYTES) {
    throw new Error(`fixture spec: expected exactly one source over ${LARGE_BYTES} bytes from the giant corpus`);
  }
  const rest = files.filter((f) => !giants.has(f.rel));
  if (rest.length !== spec.agesH.length) throw new Error(`fixture spec: ${rest.length} non-giant files but ${spec.agesH.length} ages`);
  const setAge = (abs, ageH) => { const s = (spec.baseEpochMs - ageH * HOUR_MS) / 1000; fs.utimesSync(abs, s, s); };
  setAge(giant[0].abs, spec.giantAgeH);
  rest.forEach((f, i) => setAge(f.abs, spec.agesH[(i * spec.stride) % spec.agesH.length]));
  chmodTree(home, 0o444, 0o555);

  const tree = treeDigest(home);
  const walk = LIB.walkInputs(home, spec.corpora.filter((c, i, all) => all.findIndex((d) => d.agent === c.agent) === i)
    .map((c) => ({ agent: c.agent, dataRoot: `~/${c.dataRoot}` })));
  const manifest = {
    format: 'ui-fixture/1', name: spec.name, specSha256: specSha256(spec), spec,
    generatorSha256: sha256File(benchCorpus), tree, walkInputsDigest: walk.digest,
    shape: { roots: spec.corpora.reduce((n, c) => n + c.sessions, 0), giantBytes: fs.statSync(giant[0].abs).size, ...horizonCounts(spec) },
  };
  const manifestPath = path.join(dir, 'fixture.json');
  fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 1)}\n`, { mode: 0o444 });
  return { home, manifestPath, manifest };
}

function gcd(a, b) { return b === 0 ? a : gcd(b, a % b); }

const REPO = path.resolve(HERE, '..', '..');
const real = (p) => { try { return fs.realpathSync(p); } catch { return path.resolve(p); } };
/** `child` is `parent` or lies below it. */
const within = (child, parent) => { const rel = path.relative(parent, child); return rel === '' || (!rel.startsWith(`..${path.sep}`) && rel !== '..' && !path.isAbsolute(rel)); };

/**
 * Where a fixture home may not be (a guard against `--home ~` and the like): it may not
 * be, contain or lie inside the user's home directory or the repository; its manifest
 * must sit beside it, as buildFixture writes it; and its top-level entries must be
 * exactly the spec's data roots. Only names and paths are read, never file content.
 * Returns the problems (empty: fine). `homeDir` and `repo` are parameters for tests only.
 */
export function placeProblems(home, manifestPath, { spec = FIXTURE_SPEC, homeDir = os.homedir(), repo = REPO } = {}) {
  const problems = [];
  const h = real(home);
  for (const [what, p] of [['the home directory', homeDir], ['the repository', repo]]) {
    const q = real(p);
    if (within(h, q) || within(q, h)) problems.push(`the fixture home is, contains or lies inside ${what}`);
  }
  if (problems.length) return problems;
  if (path.dirname(real(manifestPath)) !== path.dirname(h)) problems.push('the manifest is not beside the fixture home');
  const roots = [...new Set(spec.corpora.map((c) => c.dataRoot))].sort();
  let top = null;
  try { top = fs.readdirSync(h).sort(); } catch { problems.push('home unreadable'); }
  if (top && JSON.stringify(top) !== JSON.stringify(roots)) problems.push(`the home's top-level entries are not exactly the spec's data roots (${roots.join(', ')})`);
  return problems;
}

/**
 * Verify `home` against its manifest: { ok, problems, manifest, tree, wallMs }. The
 * manifest must describe FIXTURE_SPEC and the home must be in a fixture's place
 * (placeProblems), both checked before any file is read; then the tree must hash to the
 * manifest's digest.
 */
export function verifyFixture(home, manifestPath, places = {}) {
  const t0 = performance.now();
  const problems = [];
  let manifest = null;
  try { manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8')); } catch { problems.push('manifest unreadable'); }
  if (manifest) {
    if (manifest.format !== 'ui-fixture/1') problems.push('manifest format');
    if (manifest.specSha256 !== specSha256(manifest.spec) || manifest.specSha256 !== specSha256()) problems.push('spec is not FIXTURE_SPEC');
  }
  if (!problems.length) problems.push(...placeProblems(home, manifestPath, places));
  let tree = null;
  if (!problems.length) {
    try { tree = treeDigest(home); } catch { problems.push('home unreadable'); }
    if (tree && (tree.sha256 !== manifest.tree?.sha256 || tree.files !== manifest.tree?.files)) problems.push('tree digest differs from the manifest');
  }
  return { ok: problems.length === 0, problems, manifest, tree, wallMs: Math.round(performance.now() - t0) };
}
