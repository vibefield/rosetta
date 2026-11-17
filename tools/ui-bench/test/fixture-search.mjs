// The fixture's own ranked-search digest. The R1 workload (product-diagnostic-v3, run by
// r1_ranked.py inside every ui-bench run) asks for words bench-corpus never writes
// (error, sqlite, cargo, …), so on the fixture every case has 0 hits and its digest only
// proves that. This asks for words of bench-corpus's synthetic vocabulary instead, the
// same way r1_ranked.py does (a warm `serve` on the run's own db, after the run's
// digests; /api/search?q=..&limit=200 once search is ready; sha256 of the ordered
// (timeline_id, session_id, kind, at) per case; SIGINT), so ranking itself is pinned.
// The warm daemon runs in its own process group (a terminal's Ctrl-C reaches only the
// test, which stops it once), with the environment run_ui's daemon had, and every
// request and the whole search have a deadline, so a daemon that never answers cannot
// hang the test.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import http from 'node:http';

/** Fixed, like the R1 workload: changing it changes the expected digest. */
export const FIXTURE_QUERIES = [
  { label: 'broad-1', query: 'ledger' },
  { label: 'medium-1', query: 'watermark' },
  { label: 'phrase-1', query: '"assistant response"' },
  { label: 'selective-1', query: 'zeta widget' },
  { label: 'prompt-filter-1', query: 'cursor', filter: { kind: 'user' } },
  { label: 'reply-filter-1', query: 'cursor', filter: { kind: 'agent' } },
  { label: 'agent-codex-1', query: 'fold', filter: { agent: 'codex' } },
];

const sha = (s) => createHash('sha256').update(s).digest('hex');
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const REQUEST_MS = 30_000;
const DEADLINE_MS = 600_000;
function get(origin, path) {
  return new Promise((resolve, reject) => {
    const req = http.get(new URL(path, origin), (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => resolve({ status: res.statusCode, body: Buffer.concat(chunks).toString('utf8') }));
      res.on('error', reject);
    });
    req.setTimeout(REQUEST_MS, () => req.destroy(new Error(`no answer to ${path.split('?')[0]} within ${REQUEST_MS / 1000} s`)));
    req.on('error', reject);
  });
}

/** { rankedIdsSha256, perCase: [{label, hits}], warmExit } or { error }. `track(child)` lets a signal handler stop it. */
export async function fixtureSearch({ bin, db, home, track = () => {} }) {
  const env = { ...process.env };
  delete env.ROSETTA_ARTIFACT_ROOTS; // as fixture-run.mjs runs run_ui (and so its daemon)
  const child = spawn(bin, ['serve', '--db', db, '--home', home, '--agent', 'claude-code', '--agent', 'codex', '--addr', '127.0.0.1:0', '--json'],
    { stdio: ['ignore', 'pipe', 'ignore'], env, detached: true });
  track(child);
  let spawnError = null;
  const exited = new Promise((resolve) => {
    child.on('exit', (code, signal) => resolve(code ?? signal));
    child.on('error', (error) => { spawnError = error; resolve('spawn-error'); }); // never spawned: no exit follows
  });
  const t0 = Date.now();
  const late = () => Date.now() - t0 > DEADLINE_MS;
  let origin = null;
  let pending = '';
  child.stdout.on('data', (c) => {
    pending += c.toString('utf8');
    const lines = pending.split('\n');
    pending = lines.pop();
    for (const l of lines) if (origin === null && l.startsWith('{') && l.includes('"event":"ready"')) { try { origin = JSON.parse(l).origin; } catch { /* not yet */ } }
  });
  try {
    for (let i = 0; origin === null; i += 1) {
      if (spawnError) throw new Error(`warm start could not spawn (${spawnError.code ?? spawnError.message})`);
      if (i > 1200 || child.exitCode !== null) throw new Error('warm start never reported ready');
      await sleep(50);
    }
    for (let i = 0; ; i += 1) {
      const s = await get(origin, '/api/status');
      if (s.status === 200 && JSON.parse(s.body).search?.readiness === 'ready') break;
      if (late() || child.exitCode !== null) throw new Error('search never became ready');
      await sleep(50);
    }
    const cases = [];
    for (const c of FIXTURE_QUERIES) {
      if (late()) throw new Error(`the fixture search took over ${DEADLINE_MS / 1000} s`);
      const r = await get(origin, `/api/search?${new URLSearchParams({ q: c.query, limit: '200', ...(c.filter || {}) })}`);
      if (r.status !== 200) throw new Error(`${c.label}: HTTP ${r.status}`);
      const v = JSON.parse(r.body);
      if (v.state?.readiness !== 'ready') throw new Error(`${c.label}: search not ready`);
      cases.push({ label: c.label, ids: v.hits.map((x) => [x.timeline_id ?? null, x.session_id ?? null, x.kind ?? null, x.at ?? null]) });
    }
    return { rankedIdsSha256: sha(JSON.stringify(cases.map(({ label, ids }) => ({ hits: ids, label })))),
      perCase: cases.map(({ label, ids }) => ({ label, hits: ids.length })), warmExit: await stop(child, exited) };
  } catch (error) {
    await stop(child, exited);
    return { error: error.message };
  }
}

async function stop(child, exited) {
  if (child.pid === undefined) return exited; // never spawned
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGINT');
  const t = setTimeout(() => { try { child.kill('SIGKILL'); } catch { /* gone */ } }, 120_000);
  const code = await exited;
  clearTimeout(t);
  return code;
}
