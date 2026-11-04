// fixture.mjs's guard on where a fixture home may be (run_ui.mjs --home): temp
// directories stand in for the user's home directory and the repository, so no test
// ever names a live home. The refusals come before any file under the home is read.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { FIXTURE_SPEC, placeProblems, specSha256, treeDigest, verifyFixture } from '../fixture.mjs';

function layout() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ui-fixture-place-'));
  const places = { homeDir: path.join(root, 'user-home'), repo: path.join(root, 'repo') };
  for (const d of [places.homeDir, places.repo]) fs.mkdirSync(d);
  const make = (dir) => {
    const home = path.join(dir, 'home');
    for (const r of ['.claude', '.codex']) fs.mkdirSync(path.join(home, r), { recursive: true });
    fs.writeFileSync(path.join(home, '.claude', 'a.jsonl'), '{}\n');
    const manifestPath = path.join(dir, 'fixture.json');
    fs.writeFileSync(manifestPath, JSON.stringify({ format: 'ui-fixture/1', spec: FIXTURE_SPEC, specSha256: specSha256(), tree: treeDigest(home) }));
    return { home, manifestPath };
  };
  return { root, places, make };
}

test('a fixture home in its place verifies; one in or around the home directory or the repo is refused', () => {
  const { root, places, make } = layout();
  try {
    const ok = make(path.join(root, 'work', 'fixture'));
    assert.deepEqual(placeProblems(ok.home, ok.manifestPath, places), []);
    assert.equal(verifyFixture(ok.home, ok.manifestPath, places).ok, true);
    const inside = make(path.join(places.homeDir, 'scratch'));
    assert.match(placeProblems(inside.home, inside.manifestPath, places).join(), /lies inside the home directory/);
    const inRepo = make(path.join(places.repo, 'tmp'));
    assert.match(placeProblems(inRepo.home, inRepo.manifestPath, places).join(), /lies inside the repository/);
    // `--home ~` and `--home /Users`: the home directory itself, or a directory holding it.
    assert.match(placeProblems(places.homeDir, ok.manifestPath, places).join(), /the home directory/);
    assert.match(placeProblems(root, ok.manifestPath, places).join(), /the home directory/);
    // Refused before anything under it is hashed.
    const v = verifyFixture(places.homeDir, ok.manifestPath, places);
    assert.equal(v.ok, false);
    assert.equal(v.tree, null);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('the manifest must sit beside the home, and the home holds only the spec\'s data roots', () => {
  const { root, places, make } = layout();
  try {
    const a = make(path.join(root, 'a'));
    const b = make(path.join(root, 'b'));
    assert.deepEqual(placeProblems(a.home, b.manifestPath, places), ['the manifest is not beside the fixture home']);
    fs.mkdirSync(path.join(a.home, 'Library'));
    assert.match(placeProblems(a.home, a.manifestPath, places).join(), /top-level entries are not exactly the spec's data roots/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('a manifest that is not FIXTURE_SPEC\'s is refused before the home is looked at', () => {
  const { root, places, make } = layout();
  try {
    const a = make(path.join(root, 'a'));
    fs.writeFileSync(a.manifestPath, JSON.stringify({ format: 'ui-fixture/1', spec: { ...FIXTURE_SPEC, stride: 9 }, specSha256: specSha256({ ...FIXTURE_SPEC, stride: 9 }) }));
    const v = verifyFixture(path.join(root, 'no-such-home'), a.manifestPath, places);
    assert.deepEqual(v.problems, ['spec is not FIXTURE_SPEC']);
    assert.equal(v.tree, null);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
