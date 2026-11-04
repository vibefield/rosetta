// probe.js's in-page detectors, judged on the export of probe-dom.electron.mjs (a
// hidden Electron window, fixtures only): each planted breach must be counted, and
// nothing the page knew by name may reach the export.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('./probe-dom.electron.mjs', import.meta.url));
let electron = null;
try { electron = createRequire(import.meta.url)('electron'); } catch { electron = null; }

test('probe.js counts every planted breach in a real page and exports no name', { skip: electron === null ? 'electron is not installed' : false, timeout: 90_000 }, () => {
  const r = spawnSync(electron, [script], { encoding: 'utf8', timeout: 80_000 });
  const line = (r.stdout || '').split('\n').find((l) => l.startsWith('PROBE-DOM '));
  assert.ok(line, `no export (exit ${r.status}): ${(r.stderr || '').slice(-400)}`);
  const { ok, nonce, result: p, events } = JSON.parse(line.slice('PROBE-DOM '.length));
  assert.equal(ok, true);
  assert.equal(nonce, 'nonce-1', 'the result answers the finish that asked for it');
  for (const k of ['firstMetaAt']) assert.ok(p.selfTest[k] > 0, k);
  assert.deepEqual({ ws: p.selfTest.wsWrapped, fetch: p.selfTest.fetchWrapped, worker: p.selfTest.worker }, { ws: true, fetch: true, worker: 'worker' });
  assert.ok(p.h1 !== null && p.frozen !== null, 'columns up and the sets frozen');
  // I1: one of each.
  assert.equal(p.i1.c1Before, 1, 'bravo drawn ready with no frame behind it');
  assert.equal(p.i1.c2, 1, 'alpha reverted once');
  assert.equal(p.i1.c3, 1, 'data selected a project');
  assert.equal(p.i1.c4, 1, 'an indexed session flashed empty, then showed content');
  assert.equal(p.i1.c7, 1, 'the pending face named another course');
  assert.ok(p.i1.checkedRows >= 3 && p.i1.c3Frames > 0 && p.i1.c4Opens === 1 && p.i1.c7Faces === 1, 'and each says what it examined');
  // H5: alpha and delta were drawn not imported when their flips arrived, then ready.
  assert.equal(p.h5.length, 2);
  for (const [a, b] of p.h5) assert.ok(b >= a);
  // H7: the spacer and the swap moved the project rows with no input; the selected
  // card moved twice; the aim at proj-one met proj-two.
  assert.equal(p.h7.projects.moves, 4);
  assert.equal(p.h7.projects.px, 200);
  assert.equal(p.h7.selectionJumps, 2);
  assert.equal(p.h7.aims.length, 1);
  assert.deepEqual(p.h7.aims[0].slice(1), ['newest-project', 0, 60, 'other-row']);
  assert.ok(events.includes('h24') && events.includes('all-readable'));
  // Names stay in the page: no session, project, runtime, native id, title or cwd.
  const text = JSON.stringify(p);
  for (const name of ['sess-', 'proj-', 'runtime-x', 'native-', 'title of', '/private/cwd']) assert.ok(!text.includes(name), name);
});
