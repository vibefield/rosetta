// The home a ui-bench run's daemon reads, verified before the run, and what the run
// document says about it. Two kinds:
//   --snapshot SNAP              the frozen R1 corpus (private, never committed): SNAP/home,
//                                verified by the R1 kit's verify_r1_snapshot.js; identity
//                                `snapshot` (walk-inputs digest, manifest sha256, totals);
//   --home DIR --fixture FILE    a synthetic fixture home (fixture.mjs, regenerated at
//                                test time): refused before any file is read unless its
//                                manifest is FIXTURE_SPEC's and the home is in a fixture's
//                                place (never the user's home or the repo, the manifest
//                                beside it, only the spec's data roots in it), then verified
//                                against the manifest's tree digest; identity `fixture`
//                                (spec, seeds, tree digest, generator).
// The other identity field is null. Both kinds name their newest giant (D2 c) the same
// way, from the claude-code transcripts' sizes and mtimes, in memory only.
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { LARGE_BYTES, verifyFixture } from './fixture.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const R1 = path.join(HERE, '..', 'r1-bench');

/** D2(c): the newest big tree's root native id under `home`, in memory only. */
function newestGiant(home, dataRoot) {
  const lib = createRequire(path.join(R1, 'package.json'))('./r1_snapshot_lib.js');
  const { entries } = lib.walkInputs(home, [{ agent: 'claude-code', dataRoot }]);
  const trees = new Map();
  for (const e of entries) {
    const m = e.rel.match(/^projects\/([^/]+)\/([0-9a-f-]{36})(\.jsonl$|\/)/);
    if (!m) continue;
    const key = `${m[1]}/${m[2]}`;
    const t = trees.get(key) || { native: m[2], k: 0, max: 0, bytes: 0 };
    t.k = Math.max(t.k, e.mtimeMs); t.max = Math.max(t.max, e.size); t.bytes += e.size;
    trees.set(key, t);
  }
  const big = [...trees.values()].filter((t) => t.max > LARGE_BYTES).sort((x, y) => y.k - x.k);
  return big.length ? { native: big[0].native, treeBytes: big[0].bytes } : { native: null, treeBytes: null };
}

/** The frozen R1 snapshot, exactly as run_ui.mjs has always read it. */
function snapshotSource(snap) {
  const verify = spawnSync(process.execPath, [path.join(R1, 'verify_r1_snapshot.js'), snap], { encoding: 'utf8', maxBuffer: 1 << 24 });
  const verified = JSON.parse(verify.stdout.trim().split('\n').pop() || '{}');
  if (verify.status !== 0 || verified.ok !== true) throw new Error(`snapshot verification failed: ${(verified.problems || []).length} problems`);
  const meta = JSON.parse(readFileSync(path.join(snap, 'snapshot.json'), 'utf8'));
  return {
    kind: 'snapshot', home: path.join(snap, 'home'), root: snap, privatePaths: [snap],
    identity: {
      snapshot: { walkInputsDigest: meta.walkInputsDigest, manifestSha256: meta.manifestSha256, files: meta.totals?.files ?? verified.files, bytes: meta.totals?.bytes ?? verified.bytes },
      fixture: null,
    },
    verify: { ok: verified.ok, files: verified.files, bytes: verified.bytes, wallMs: verified.wallMs },
    giant: meta.agents['claude-code'] ? newestGiant(path.join(snap, 'home'), meta.agents['claude-code'].dataRoot) : { native: null, treeBytes: null },
  };
}

/** A fixture home and its manifest (fixture.mjs). `idleGuard` is declared in the identity. */
function fixtureSource(home, manifestPath, idleGuard) {
  const v = verifyFixture(home, manifestPath);
  if (!v.ok) throw new Error(`fixture verification failed: ${v.problems.join('; ')}`);
  const { spec } = v.manifest;
  return {
    kind: 'fixture', home, root: home, privatePaths: [home, path.dirname(manifestPath), manifestPath],
    identity: {
      snapshot: null,
      fixture: {
        name: v.manifest.name, specSha256: v.manifest.specSha256, treeSha256: v.tree.sha256, files: v.tree.files, bytes: v.tree.bytes,
        walkInputsDigest: v.manifest.walkInputsDigest, generatorSha256: v.manifest.generatorSha256,
        baseEpochMs: spec.baseEpochMs, giantAgeH: spec.giantAgeH, agesH: spec.agesH, stride: spec.stride,
        corpora: spec.corpora.map(({ agent, sessions, lines, seed, toolRatio, giant }) => ({ agent, sessions, lines, seed, toolRatio, giant })),
        verifyWallMs: v.wallMs, idleGuard,
      },
    },
    verify: null,
    giant: newestGiant(home, '~/.claude'),
  };
}

/** The run's source from parsed arguments: { kind, home, root, privatePaths, identity, verify, giant }. */
export function prepareSource(a) {
  return a.snap ? snapshotSource(path.resolve(a.snap)) : fixtureSource(path.resolve(a.home), path.resolve(a.fixture), a.idleGuard);
}
