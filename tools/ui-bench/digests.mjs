// The run's equality digests, all read-only, by the R1 kit's own files (by path):
// the logical and cursor-order digest SQL (`file:...?mode=ro&immutable=1`), `ledger
// verify --json` sanitized as pair_r1.sh does (no ledger path; segment lists as
// counts), and r1_ranked.py's ranked-result digest (a warm start of the run's own db).
// The outputs land in OUT/digests (digests and counts only); the run JSON keeps the
// totals and a sha256 of each output, so a differing section can be found later.
// Also the forbidden-value list for the privacy gate, read from the same db.
//
// Every child runs asynchronously and is registered with the caller's `track`, so a
// signal handler can stop it; r1_ranked.py is stopped with SIGINT (its `finally`
// then SIGINTs and reaps the warm daemon it started), never SIGTERM. A failure is
// logged as an exit status and a fixed reason; raw stderr (it can carry paths) goes
// only to `rawLog`, a file the driver deletes unless --keep.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';

const sha = (text) => createHash('sha256').update(text).digest('hex');
// r1_ranked.py's own worst case is 300 s to ready + 900 s to search-ready + its
// queries; this bound sits above the first two, and after it the script gets SIGINT
// and RANKED_GRACE_MS to clean up before SIGKILL.
export const RANKED_TIMEOUT_MS = 25 * 60_000;
const RANKED_GRACE_MS = 180_000;

/** Run a child to completion: { status, signal, stdout, stderr, timedOut }. */
export function runChild(cmd, args, { input = null, timeoutMs = 120_000, killSignal = 'SIGTERM', graceMs = 10_000, track = () => {}, maxBytes = 1 << 26 } = {}) {
  return new Promise((resolve) => {
    let child;
    try { child = spawn(cmd, args, { stdio: [input === null ? 'ignore' : 'pipe', 'pipe', 'pipe'] }); } catch (error) {
      resolve({ status: null, signal: null, stdout: '', stderr: String(error?.message ?? error), timedOut: false });
      return;
    }
    const untrack = track(child, killSignal) || (() => {});
    const out = [];
    const err = [];
    let bytes = 0;
    let timedOut = false;
    let kill = null;
    child.stdout.on('data', (c) => { bytes += c.length; if (bytes <= maxBytes) out.push(c); });
    child.stderr.on('data', (c) => { if (err.length < 64) err.push(c); });
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill(killSignal);
      kill = setTimeout(() => child.kill('SIGKILL'), graceMs);
    }, timeoutMs);
    child.on('error', () => {});
    child.on('close', (status, signal) => {
      clearTimeout(timer);
      if (kill !== null) clearTimeout(kill);
      untrack();
      resolve({ status, signal, stdout: Buffer.concat(out).toString('utf8'), stderr: Buffer.concat(err).toString('utf8'), timedOut });
    });
    if (input !== null) { child.stdin.on('error', () => {}); child.stdin.end(input); }
  });
}

/** Parse r1_logical_digest.sql output: section|component|rows|sha, count|name|value. */
export function parseLogical(text) {
  const totals = {};
  const counts = {};
  for (const line of text.split('\n')) {
    const p = line.split('|');
    if (p[0] === 'count' && p.length === 3) counts[p[1]] = Number(p[2]);
    else if (p.length === 4 && (p[1] === 'TOTAL' || p[1] === 'TOTAL_R')) totals[`${p[0]}:${p[1]}`] = p[3];
  }
  return { totals, counts };
}

export function sanitizeVerify(stdout) {
  const lines = stdout.split('\n').filter((l) => l.startsWith('{'));
  if (!lines.length) return null;
  const v = JSON.parse(lines[lines.length - 1]);
  delete v.ledger;
  for (const r of v.runtimes ?? []) {
    const segs = r.segments;
    delete r.segments;
    r.segmentCount = Array.isArray(segs) ? segs.length : segs ?? null;
  }
  return v;
}

const sqliteUri = (db) => `file:${db}?mode=ro&immutable=1`;

// Every private string the run's projection holds about the corpus, one per line
// (newlines inside a value are flattened to separate lines, each also forbidden).
const FORBIDDEN_SQL = `
SELECT id FROM session UNION ALL SELECT native_session_id FROM session UNION ALL
SELECT runtime_id FROM session UNION ALL SELECT project_id FROM session WHERE project_id IS NOT NULL UNION ALL
SELECT cwd FROM session WHERE cwd IS NOT NULL UNION ALL SELECT parent_session_id FROM session WHERE parent_session_id IS NOT NULL UNION ALL
SELECT root_session_id FROM session UNION ALL SELECT title FROM session WHERE title IS NOT NULL UNION ALL
SELECT summary FROM session WHERE summary IS NOT NULL UNION ALL
SELECT id FROM project UNION ALL SELECT name FROM project UNION ALL SELECT root_paths FROM project UNION ALL
SELECT j.value FROM project, json_each(CASE WHEN json_valid(project.root_paths) AND json_type(project.root_paths) = 'array' THEN project.root_paths ELSE '[]' END) AS j UNION ALL
SELECT git_repo FROM project WHERE git_repo IS NOT NULL UNION ALL
SELECT id FROM agent_runtime UNION ALL SELECT data_root FROM agent_runtime UNION ALL SELECT account FROM agent_runtime WHERE account IS NOT NULL UNION ALL
SELECT timeline_id FROM message UNION ALL SELECT group_id FROM message;
SELECT 'END-OF-FORBIDDEN';
`;

/** The forbidden-value list from the run's own db (read-only), with the source's
 * row count, or { ok: false } when it cannot be read completely. */
export async function forbiddenFromDb({ db, track }) {
  if (!existsSync(db)) return { ok: false, values: [], reason: 'no-db' };
  const r = await runChild('/usr/bin/sqlite3', ['-readonly', '-noheader', '-list', sqliteUri(db)], { input: FORBIDDEN_SQL, timeoutMs: 300_000, track, maxBytes: 1 << 30 });
  const lines = r.stdout.split('\n');
  if (r.status !== 0 || r.timedOut || !lines.includes('END-OF-FORBIDDEN')) return { ok: false, values: [], reason: 'query-failed' };
  const values = new Set();
  for (const line of lines) if (line && line !== 'END-OF-FORBIDDEN') values.add(line);
  return { ok: true, values: [...values], reason: null };
}

export async function digestRun({ bin, db, snapHome, outDir, r1, log, track, rawLog }) {
  mkdirSync(outDir, { recursive: true, mode: 0o700 });
  const raw = (what, r) => { try { appendFileSync(rawLog, `== ${what} (exit ${r.status}${r.signal ? ` ${r.signal}` : ''}${r.timedOut ? ' timeout' : ''})\n${r.stderr}\n`, { mode: 0o600 }); } catch { /* best effort */ } };
  const failed = (what, r) => { log(`digests: ${what} failed (exit ${r.status ?? r.signal ?? 'none'}${r.timedOut ? ', timed out' : ''})`); raw(what, r); };
  const out = { logicalTotal: null, strictTotal: null, strictNoidTotal: null, strictNoidRTotal: null, surrogateTotal: null,
    logicalOutSha256: null, cursorOrderOutSha256: null, counts: null, sourceSetDigest: null, sourceDigest: null, metaDigest: null,
    entryDigest: null, ledgerVerifyOk: false, rankedIdsSha256: null, rankedSha256: null, rankedWarmExit: null };
  if (!existsSync(db)) { log('digests: no db'); return out; }
  const sql = (file) => runChild('/usr/bin/sqlite3', ['-readonly', sqliteUri(db)], { input: readFileSync(path.join(r1, file)), timeoutMs: 600_000, track });
  const logical = await sql('r1_logical_digest.sql');
  if (logical.status === 0) {
    writeFileSync(path.join(outDir, 'r1_logical_digest.out'), logical.stdout);
    const { totals, counts } = parseLogical(logical.stdout);
    Object.assign(out, { logicalTotal: totals['logical:TOTAL'] ?? null, strictTotal: totals['strict:TOTAL'] ?? null,
      strictNoidTotal: totals['strict_noid:TOTAL'] ?? null, strictNoidRTotal: totals['strict_noid:TOTAL_R'] ?? null,
      surrogateTotal: totals['surrogate:TOTAL'] ?? null, logicalOutSha256: sha(logical.stdout), counts });
  } else failed('logical digest', logical);
  const cursor = await sql('r1_cursor_order.sql');
  if (cursor.status === 0) {
    writeFileSync(path.join(outDir, 'r1_cursor_order.out'), cursor.stdout);
    out.cursorOrderOutSha256 = sha(cursor.stdout);
  } else failed('cursor-order diagnostic', cursor);
  const verify = await runChild(bin, ['ledger', 'verify', '--db', db, '--json'], { timeoutMs: 600_000, killSignal: 'SIGINT', track });
  let v = null;
  try { v = verify.status === 0 ? sanitizeVerify(verify.stdout) : null; } catch { v = null; }
  if (v) {
    writeFileSync(path.join(outDir, 'ledger_verify.json'), `${JSON.stringify(v, null, 1)}\n`);
    Object.assign(out, { sourceSetDigest: v.sourceSetDigest ?? null, sourceDigest: v.sourceDigest ?? null, metaDigest: v.metaDigest ?? null,
      entryDigest: v.entryDigest ?? null, ledgerVerifyOk: true });
  } else failed('ledger verify', verify);
  const rankedPath = path.join(outDir, 'ranked.json');
  const ranked = await runChild('python3', ['-I', path.join(r1, 'r1_ranked.py'), bin, db, snapHome, rankedPath],
    { timeoutMs: RANKED_TIMEOUT_MS, killSignal: 'SIGINT', graceMs: RANKED_GRACE_MS, track });
  let r = null;
  try { r = existsSync(rankedPath) ? JSON.parse(readFileSync(rankedPath, 'utf8')) : null; } catch { r = null; }
  if (r) Object.assign(out, { rankedIdsSha256: r.ranked_ids_sha256 ?? null, rankedSha256: r.ranked_sha256 ?? null, rankedWarmExit: r.warm_exit ?? null });
  if (!r || ranked.status !== 0) failed('ranked digest', ranked);
  return out;
}
