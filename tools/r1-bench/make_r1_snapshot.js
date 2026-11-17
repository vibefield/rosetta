#!/usr/bin/env node
'use strict';
// R1 frozen corpus (brief v3 §12 E1 steps 1-7), from imgdefer/make_snapshot.js.
// safeJoin, stableStat, sameStat, sha256File, copyStableAndVerify and
// directoriesBelow are kept verbatim; the selection is ALL admitted Claude
// Code and Codex sources (every driver), one copy per physical file.
// Prints sizes/counts only: no source paths, no transcript content.
//
// usage: node make_r1_snapshot.js B0_BINARY SOURCE_HOME OUT_DIR DRY_DIR
//   OUT_DIR  the snapshot (OUT/home/<dataRoot without ~>/..., SHA256SUMS, snapshot.json)
//   DRY_DIR  scratch for the step-5 Codex state-member dry run (deleted after it)
//
// Order: 1 inventory, 2 regular files, 3 SQLite (.backup + journal_mode=DELETE),
// 7a freeze OUT/home (dirs 0555), 4 admission check and 5 dry run ON THE FROZEN
// home (the dry run is what proves a read-only open works inside 0555
// directories), 6 manifest, 7b freeze OUT.
//
// SQLite deviation from the brief's `sqlite3 -readonly SRC ".backup DST"`,
// recorded in snapshot.json: the db and its present sidecars are first copied
// byte-for-byte (copyStableAndVerify, group-stable) into a private staging
// directory and `.backup` reads the staged copy, so sqlite never opens the
// user's live file (a read-only WAL open can still write its -shm).

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { spawnSync } = require('node:child_process');
const lib = require('./r1_snapshot_lib.js');

const [binary, sourceHome, outDir, dryDir] = process.argv.slice(2);
if (!binary || !sourceHome || !outDir || !dryDir) {
  console.error('usage: make_r1_snapshot.js B0_BINARY SOURCE_HOME OUT_DIR DRY_DIR');
  process.exit(2);
}
for (const d of [outDir, dryDir]) {
  if (fs.existsSync(d)) { console.error('output directory already exists; refusing to overwrite'); process.exit(2); }
}
const AGENTS = ['claude-code', 'codex'];
const SQLITE = '/usr/bin/sqlite3';
const MAX_ATTEMPTS = 10;
const LARGE_BYTES = 64 * 1024 * 1024;

function sleepMs(ms) { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms); }
function wallMs(t) { return Math.round(Number(process.hrtime.bigint() - t) / 1e6); }

function sources(home, agent) {
  const run = spawnSync(binary, ['sources', '--home', home, '--agent', agent, '--json'], {
    encoding: 'utf8', maxBuffer: 256 * 1024 * 1024,
  });
  if (run.status !== 0) throw new Error(`rosettad sources exited ${run.status} for ${agent}`);
  return run.stdout.split('\n').filter(Boolean).map((line) => JSON.parse(line))
    .filter((event) => event.event === 'source');
}

// --- verbatim helpers from imgdefer/make_snapshot.js (profile-cold-start.js) -
function safeJoin(root, portableRelative) {
  const resolvedRoot = path.resolve(root);
  const target = path.resolve(resolvedRoot, ...portableRelative.split('/'));
  if (target !== resolvedRoot && !target.startsWith(`${resolvedRoot}${path.sep}`)) {
    throw new Error('source path escaped its runtime root');
  }
  return target;
}
function expandDataRoot(dataRoot, home) {
  if (dataRoot === '~') return home;
  if (dataRoot.startsWith('~/')) return path.join(home, dataRoot.slice(2));
  throw new Error(`the isolated profiler requires a home-relative data root, got ${dataRoot}`);
}
function stableStat(file) {
  const stat = fs.statSync(file, { bigint: true });
  if (!stat.isFile()) throw new Error('selected source is not a regular file');
  return { dev: stat.dev, ino: stat.ino, size: stat.size, mtimeNs: stat.mtimeNs };
}
function sameStat(l, r) {
  return l.dev === r.dev && l.ino === r.ino && l.size === r.size && l.mtimeNs === r.mtimeNs;
}
function sha256File(file) {
  const hash = createHash('sha256');
  const fd = fs.openSync(file, 'r');
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    let offset = 0;
    while (true) {
      const n = fs.readSync(fd, buffer, 0, buffer.length, offset);
      if (n === 0) break;
      hash.update(buffer.subarray(0, n));
      offset += n;
    }
    return hash.digest('hex');
  } finally { fs.closeSync(fd); }
}
function copyStableAndVerify(source, destination, expectedBytes) {
  const before = stableStat(source);
  if (Number(before.size) !== expectedBytes) throw new Error('source size changed since inventory');
  fs.mkdirSync(path.dirname(destination), { recursive: true, mode: 0o700 });
  const input = fs.openSync(source, 'r');
  const output = fs.openSync(destination, 'wx', 0o600);
  const hash = createHash('sha256');
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    let readOffset = 0;
    while (true) {
      const n = fs.readSync(input, buffer, 0, buffer.length, readOffset);
      if (n === 0) break;
      hash.update(buffer.subarray(0, n));
      let written = 0;
      while (written < n) written += fs.writeSync(output, buffer, written, n - written, readOffset + written);
      readOffset += n;
    }
    fs.fsyncSync(output);
  } finally { fs.closeSync(input); fs.closeSync(output); }
  const after = stableStat(source);
  if (!sameStat(before, after)) throw new Error('source changed while it was frozen');
  const sourceDigest = hash.digest('hex');
  const copyDigest = sha256File(destination);
  if (sourceDigest !== copyDigest) throw new Error('snapshot checksum mismatch');
  const modified = new Date(Number(before.mtimeNs / 1_000_000n));
  fs.utimesSync(destination, modified, modified);
  fs.chmodSync(destination, 0o444);
  return copyDigest;
}
function directoriesBelow(root) {
  const out = [];
  (function visit(d) {
    out.push(d);
    for (const e of fs.readdirSync(d, { withFileTypes: true })) if (e.isDirectory()) visit(path.join(d, e.name));
  })(root);
  return out;
}
// -------------------------------------------------------------------------

const RETRYABLE = /source changed while it was frozen|source size changed since inventory/;
function removeIfPresent(file) {
  try { fs.chmodSync(file, 0o600); fs.unlinkSync(file); } catch (error) { if (error.code !== 'ENOENT') throw error; }
}

// Step 2: one regular file, retried up to 10 times 1 s apart on instability.
// The inventory size is the selection's size; a live file that grew since is
// copied at its stable size then (counted, never named).
function copyRegular(from, to) {
  for (let attempt = 1; ; attempt += 1) {
    const current = Number(stableStat(from).size);
    try {
      const sha = copyStableAndVerify(from, to, current);
      const st = fs.statSync(to, { bigint: true });
      return { sha, attempts: attempt, bytes: Number(st.size), mtimeMs: Number(st.mtimeNs / 1_000_000n) };
    } catch (error) {
      if (!RETRYABLE.test(error.message) || attempt >= MAX_ATTEMPTS) throw error;
      removeIfPresent(to);
      sleepMs(1000);
    }
  }
}

function sqlite(args, label) {
  const run = spawnSync(SQLITE, args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  if (run.status !== 0 || run.stderr.trim()) throw new Error(`sqlite3 ${label} failed (exit ${run.status})`);
  return run.stdout.trim();
}

// Step 3: a SQLite source. Stage db + present sidecars group-stably, .backup
// the staged copy to DST, PRAGMA journal_mode=DELETE, integrity_check ok,
// journal_mode reads back delete, no DST-wal/-shm/-journal, sha, mtime, 0444.
function freezeSqlite(fromDb, sidecars, toDb, staging) {
  for (let attempt = 1; ; attempt += 1) {
    const stage = path.join(staging, `attempt-${attempt}`);
    fs.mkdirSync(stage, { recursive: true, mode: 0o700 });
    const group = [fromDb, ...sidecars.filter((s) => fs.existsSync(s))];
    const before = group.map(stableStat);
    let stable = true;
    const staged = [];
    try {
      group.forEach((file, i) => {
        const dst = path.join(stage, `db${file.slice(fromDb.length)}`);
        copyStableAndVerify(file, dst, Number(before[i].size));
        fs.chmodSync(dst, 0o600);
        staged.push(dst);
      });
      const after = group.map(stableStat);
      stable = before.every((b, i) => sameStat(b, after[i]));
    } catch (error) {
      if (!RETRYABLE.test(error.message)) throw error;
      stable = false;
    }
    if (!stable) {
      fs.rmSync(stage, { recursive: true, force: true });
      if (attempt >= MAX_ATTEMPTS) throw new Error('sqlite source changed while it was frozen');
      sleepMs(1000);
      continue;
    }
    const stagedDb = staged[0];
    const sourceJournalMode = sqlite([stagedDb, 'PRAGMA journal_mode;'], 'journal_mode (staged)');
    fs.mkdirSync(path.dirname(toDb), { recursive: true, mode: 0o700 });
    sqlite([stagedDb, `.backup '${toDb.replace(/'/g, "''")}'`], '.backup');
    const converted = sqlite([toDb, 'PRAGMA journal_mode=DELETE;'], 'journal_mode=DELETE');
    if (converted !== 'delete') throw new Error(`journal_mode=DELETE answered ${converted}`);
    // macOS's /usr/bin/sqlite3 persists WAL sidecars (SQLITE_FCNTL_PERSIST_WAL
    // default): the conversion leaves a stale -shm behind. With the mode
    // DELETE and no -wal it is dead state; remove it, then assert none.
    if (fs.existsSync(`${toDb}-wal`)) throw new Error('converted snapshot db kept a -wal sidecar');
    const staleShm = fs.existsSync(`${toDb}-shm`);
    if (staleShm) fs.unlinkSync(`${toDb}-shm`);
    const integrity = sqlite(['-readonly', toDb, 'PRAGMA integrity_check;'], 'integrity_check');
    if (integrity !== 'ok') throw new Error('integrity_check is not ok');
    const readback = sqlite(['-readonly', toDb, 'PRAGMA journal_mode;'], 'journal_mode readback');
    if (readback !== 'delete') throw new Error(`journal_mode reads back ${readback}`);
    for (const suffix of ['-wal', '-shm', '-journal']) {
      if (fs.existsSync(toDb + suffix)) throw new Error(`converted snapshot db has a ${suffix} sidecar`);
    }
    const uri = `file:${toDb}?mode=ro&immutable=1`;
    const members = Number(sqlite(['-readonly', uri,
      "SELECT count(*) FROM threads WHERE id IS NOT NULL AND trim(id) <> '';"], 'member count'));
    const threads = Number(sqlite(['-readonly', uri, 'SELECT count(*) FROM threads;'], 'thread count'));
    const sha = sha256File(toDb);
    const modified = new Date(Number(before[0].mtimeNs / 1_000_000n));
    fs.utimesSync(toDb, modified, modified);
    fs.chmodSync(toDb, 0o444);
    for (const suffix of ['-wal', '-shm', '-journal']) {
      if (fs.existsSync(toDb + suffix)) throw new Error(`converted snapshot db has a ${suffix} sidecar`);
    }
    fs.rmSync(stage, { recursive: true, force: true });
    const st = fs.statSync(toDb, { bigint: true });
    return {
      sha, attempts: attempt, bytes: Number(st.size), mtimeMs: Number(st.mtimeNs / 1_000_000n),
      sourceBytes: Number(before[0].size), sourceJournalMode, journalMode: readback, integrity,
      sidecarsStaged: group.length - 1, staleShmRemovedAfterConversion: staleShm, members, threads,
    };
  }
}

const SIDE = /^(.*)-(wal|shm|journal)$/;
function hoursBetween(a, b) { return Math.round(((a - b) / 3_600_000) * 100) / 100; }

// ---------------------------------------------------------------- step 1
const tInv = process.hrtime.bigint();
const inventory = AGENTS.flatMap((agent) => sources(sourceHome, agent));
const inventoryWallMs = wallMs(tInv);
const roots = {};
for (const e of inventory) {
  const r = roots[e.agent] || (roots[e.agent] = { agent: e.agent, dataRoot: e.dataRoot, resolvedRoot: e.resolvedRoot });
  if (r.dataRoot !== e.dataRoot || r.resolvedRoot !== e.resolvedRoot) throw new Error(`${e.agent} sources span data roots`);
}
for (const agent of AGENTS) if (!roots[agent]) throw new Error(`no admitted ${agent} source`);

// Group `physical#member` under the physical file; copy each physical file once.
const physical = new Map();
let memberEvents = 0;
for (const e of inventory) {
  const [file, member] = e.relativePath.split('#', 2);
  if (member !== undefined) memberEvents += 1;
  const key = `${e.agent}\u0000${file}`;
  const p = physical.get(key) || { agent: e.agent, rel: file, stream: e.stream, driver: e.driver, inventoryBytes: e.bytes,
    inventoryMtimeMs: e.modifiedMs, events: [] };
  if (p.stream !== e.stream || p.driver !== e.driver) throw new Error('one physical file admitted under two streams');
  p.events.push(e);
  physical.set(key, p);
}
// SQLite sidecars (-wal/-shm/-journal of an admitted sqlite-snapshot db) are
// hints, not content: they are staged with their db, never frozen.
for (const p of physical.values()) {
  const m = p.driver === 'sqlite-snapshot' && p.rel.match(SIDE);
  p.kind = 'regular';
  if (p.driver === 'sqlite-snapshot') p.kind = 'sqlite';
  if (m && physical.has(`${p.agent}\u0000${m[1]}`) && physical.get(`${p.agent}\u0000${m[1]}`).driver === 'sqlite-snapshot') {
    p.kind = 'sidecar';
    p.db = m[1];
  }
}

fs.mkdirSync(outDir, { recursive: false, mode: 0o700 });
const snapshotHome = path.join(outDir, 'home');
fs.mkdirSync(snapshotHome, { mode: 0o700 });
const staging = path.join(dryDir, 'sqlite-staging');
fs.mkdirSync(dryDir, { recursive: false, mode: 0o700 });

// ---------------------------------------------------------------- steps 2-3
const tCopy = process.hrtime.bigint();
const frozen = [];
const attemptsHistogram = {};
let grewSinceInventory = 0;
const sqliteReports = [];
for (const p of [...physical.values()].sort((l, r) => lib.byteCompare(`${l.agent}\u0000${l.rel}`, `${r.agent}\u0000${r.rel}`))) {
  if (p.kind === 'sidecar') continue;
  const root = roots[p.agent];
  const snapRoot = expandDataRoot(root.dataRoot, snapshotHome);
  const from = safeJoin(root.resolvedRoot, p.rel);
  const to = safeJoin(snapRoot, p.rel);
  let done;
  if (p.kind === 'sqlite') {
    const sidecars = ['-wal', '-shm', '-journal'].map((s) => from + s);
    done = freezeSqlite(from, sidecars, to, staging);
    sqliteReports.push({
      agent: p.agent, stream: p.stream, bytes: done.bytes, sourceBytes: done.sourceBytes,
      sourceJournalMode: done.sourceJournalMode, journalMode: done.journalMode, integrityCheck: done.integrity,
      sidecarsStaged: done.sidecarsStaged, staleShmRemovedAfterConversion: done.staleShmRemovedAfterConversion, sidecarsInSnapshot: 0, threads: done.threads, members: done.members,
      inventoryMemberEvents: p.events.filter((e) => e.relativePath.includes('#')).length,
    });
  } else {
    done = copyRegular(from, to);
    if (done.bytes !== p.inventoryBytes) grewSinceInventory += 1;
  }
  attemptsHistogram[done.attempts] = (attemptsHistogram[done.attempts] || 0) + 1;
  frozen.push({ ...p, abs: to, manifestPath: path.relative(outDir, to).split(path.sep).join('/'),
    sha: done.sha, bytes: done.bytes, mtimeMs: done.mtimeMs, attempts: done.attempts });
}
const copyWallMs = wallMs(tCopy);
fs.rmSync(staging, { recursive: true, force: true });

// ---------------------------------------------------------------- step 7a
for (const d of directoriesBelow(snapshotHome).reverse()) fs.chmodSync(d, 0o555);

// ---------------------------------------------------------------- step 4
const tAdm = process.hrtime.bigint();
const admitted = AGENTS.flatMap((agent) => sources(snapshotHome, agent));
const admissionWallMs = wallMs(tAdm);
const key = (e) => `${e.agent}\t${e.relativePath}\t${e.stream}\t${e.driver}`;
const expected = new Map();
for (const p of physical.values()) {
  if (p.kind === 'sidecar') continue;
  for (const e of p.events) expected.set(key(e), (expected.get(key(e)) || 0) + 1);
}
const got = new Map();
for (const e of admitted) got.set(key(e), (got.get(key(e)) || 0) + 1);
const missing = [...expected].filter(([k, n]) => got.get(k) !== n).length;
const extra = [...got].filter(([k]) => !expected.has(k)).length;
if (missing || extra) throw new Error(`admission multiset differs: ${missing} missing/miscounted, ${extra} extra`);
const frozenByKey = new Map(frozen.map((f) => [`${f.agent}\u0000${f.rel}`, f]));
let byteMismatch = 0;
let mtimeMismatch = 0;
for (const e of admitted) {
  const f = frozenByKey.get(`${e.agent}\u0000${e.relativePath.split('#')[0]}`);
  if (!f) throw new Error('admitted a source that was not frozen');
  if (e.bytes !== f.bytes) byteMismatch += 1;
  if (e.modifiedMs !== f.mtimeMs) mtimeMismatch += 1;
}
if (byteMismatch) throw new Error(`${byteMismatch} admitted sources differ in bytes from the frozen copy`);
const sidecarsExcluded = [...physical.values()].filter((p) => p.kind === 'sidecar').length;

// ---------------------------------------------------------------- step 5
const dryDb = path.join(dryDir, 'db', 'rosetta.sqlite');
fs.mkdirSync(path.dirname(dryDb), { recursive: true, mode: 0o700 });
const tDry = process.hrtime.bigint();
const dry = spawnSync(binary, ['serve', '--home', snapshotHome, '--agent', 'codex', '--ticks', '0', '--json', '--profile',
  '--db', dryDb, '--addr', '127.0.0.1:0'], { encoding: 'utf8', maxBuffer: 512 * 1024 * 1024, timeout: 15 * 60 * 1000 });
const dryWallMs = wallMs(tDry);
const dryLines = (dry.stdout || '').split('\n').filter(Boolean);
const dryEvents = dryLines.filter((l) => l.startsWith('{')).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
const dryText = dryLines.filter((l) => !l.startsWith('{'));
const prepared = dryEvents.filter((e) => e.event === 'stream-prepared' && e.agent === 'codex');
const metaEvent = dryEvents.find((e) => e.event === 'meta' && e.agent === 'codex') || null;
const published = dryText.some((l) => l.startsWith('milestone: canonical-ready'));
const tolerated = dryText.filter((l) => l.startsWith('codex: tolerated per-file imperfections')).length;
const activeMembers = dry.status === 0 && fs.existsSync(dryDb) ? Number(sqlite(['-readonly', `file:${dryDb}?mode=ro&immutable=1`,
  "SELECT COUNT(*) FROM ingest_cursor WHERE path LIKE '%.sqlite#%' AND active_generation > 0;"], 'dry-run cursor count')) : null;
const memberCount = sqliteReports.filter((s) => s.agent === 'codex').reduce((n, s) => n + s.members, 0);
const dryRun = {
  binary: 'B0 3ca7410 (Task B build)',
  command: 'serve --home <SNAPSHOT>/home --agent codex --ticks 0 --json --profile --db <DRY>/db/rosetta.sqlite --addr 127.0.0.1:0',
  exitCode: dry.status,
  signal: dry.signal,
  wallMs: dryWallMs,
  checks: {
    a_publishes: dry.status === 0 && published,
    a_toleratedLinesForCodex: tolerated,
    b_streamPreparedEvents: prepared.length,
    b_filesMissing: prepared.length ? prepared.reduce((n, e) => n + e.filesMissing, 0) : null,
    b_retriesExhausted: prepared.length ? prepared.reduce((n, e) => n + e.retriesExhausted, 0) : null,
    c_activeMemberCursors: activeMembers,
    c_inventoryMemberCount: memberCount,
    d_groupsHoisted: 'pending: from the B2 walk: line (E1.8, pairs agent)',
  },
  streamPrepared: prepared.map((e) => ({ records: e.records, parsed: e.parsed, unknown: e.unknown, hardErrors: e.hardErrors,
    oversized: e.oversized, quarantined: e.quarantined, conflicts: e.conflicts, filesIncomplete: e.filesIncomplete })),
  meta: metaEvent && { sessions: metaEvent.sessions, projects: metaEvent.projects, filesMatched: metaEvent.filesMatched,
    filesSkimmed: metaEvent.filesSkimmed, filesUnidentified: metaEvent.filesUnidentified, filesUnfinished: metaEvent.filesUnfinished,
    filesVanished: metaEvent.filesVanished, walkTruncated: metaEvent.walkTruncated },
};
const c = dryRun.checks;
const dryOk = c.a_publishes && c.a_toleratedLinesForCodex === 0 && c.b_streamPreparedEvents >= 1 && c.b_filesMissing === 0
  && c.b_retriesExhausted === 0 && c.c_activeMemberCursors === c.c_inventoryMemberCount;
fs.rmSync(dryDir, { recursive: true, force: true });
if (!dryOk) {
  console.error(JSON.stringify({ dryRunFailed: dryRun }, null, 2));
  throw new Error('Codex state-member dry run failed: fix the snapshot, never weaken the check');
}

// ---------------------------------------------------------------- step 6
const sums = frozen.map((f) => ({ line: `${f.sha}  ${f.manifestPath}`, p: f.manifestPath }))
  .sort((l, r) => lib.byteCompare(l.p, r.p)).map((x) => x.line);
const manifestText = `${sums.join('\n')}\n`;
const walk = lib.walkInputs(snapshotHome, Object.values(roots).map(({ agent, dataRoot }) => ({ agent, dataRoot })));
if (walk.entries.length !== frozen.length) throw new Error('frozen home holds files outside the frozen set');
// Cross-check: the production admission's (size, modifiedMs) equal the fs view.
const walkByKey = new Map(walk.entries.map((e) => [`${e.agent}\u0000${e.rel}`, e]));
for (const f of frozen) {
  const w = walkByKey.get(`${f.agent}\u0000${f.rel}`);
  if (!w || w.size !== f.bytes || w.mtimeMs !== f.mtimeMs || w.mode !== 0o444) throw new Error('walk inputs disagree with the frozen copy');
}

const newest = Math.max(...frozen.map((f) => f.mtimeMs));
const BUCKETS = [['le1h', 3_600_000], ['le6h', 6 * 3_600_000], ['le24h', 24 * 3_600_000], ['le7d', 7 * 86_400_000],
  ['le30d', 30 * 86_400_000], ['older', Infinity]];
function ageBuckets(list) {
  const top = list.length ? Math.max(...list.map((f) => f.mtimeMs)) : 0;
  const out = Object.fromEntries(BUCKETS.map(([k]) => [k, { files: 0, bytes: 0 }]));
  for (const f of list) {
    const age = top - f.mtimeMs;
    const [k] = BUCKETS.find(([, lim]) => age <= lim);
    out[k].files += 1;
    out[k].bytes += f.bytes;
  }
  return out;
}
function countBy(list, fn) {
  const out = {};
  for (const f of list) {
    const k = fn(f);
    out[k] = out[k] || { files: 0, bytes: 0 };
    out[k].files += 1;
    out[k].bytes += f.bytes;
  }
  return out;
}
// Tree sizes, a PATH PROXY (structure only): Claude Code = a root transcript
// projects/<p>/<s>.jsonl plus everything under projects/<p>/<s>/; Codex = each
// rollout alone, the state db as one shared group. The real TreeMap (metadata
// links) comes from B2's walk-plan events in E1.8.
function treeKey(f) {
  const parts = f.rel.split('/');
  if (f.agent === 'claude-code' && parts[0] === 'projects' && parts.length >= 3) {
    return `claude-code\u0000${parts[1]}\u0000${parts.length === 3 ? parts[2].replace(/\.jsonl$/, '') : parts[2]}`;
  }
  if (f.agent === 'codex' && f.driver === 'sqlite-snapshot') return 'codex\u0000state-db';
  return `${f.agent}\u0000${f.rel}`;
}
const trees = new Map();
for (const f of frozen) {
  const k = treeKey(f);
  const t = trees.get(k) || { agent: f.agent, files: 0, bytes: 0, maxSourceBytes: 0, mtimeMs: 0, shared: f.driver === 'sqlite-snapshot' };
  t.files += 1;
  t.bytes += f.bytes;
  t.maxSourceBytes = Math.max(t.maxSourceBytes, f.bytes);
  t.mtimeMs = Math.max(t.mtimeMs, f.mtimeMs);
  trees.set(k, t);
}
const treeList = [...trees.values()];
const byAgent = Object.fromEntries(AGENTS.map((agent) => {
  const list = frozen.filter((f) => f.agent === agent);
  const tl = treeList.filter((t) => t.agent === agent && !t.shared);
  return [agent, {
    dataRoot: roots[agent].dataRoot,
    files: list.length,
    bytes: list.reduce((s, f) => s + f.bytes, 0),
    byDriver: countBy(list, (f) => f.driver),
    byStream: countBy(list, (f) => `${f.stream}:${f.driver}`),
    ageBucketsFromAgentNewest: ageBuckets(list),
    treesPathProxy: tl.length,
    treesOverLargeBytesPathProxy: tl.filter((t) => t.maxSourceBytes > LARGE_BYTES).length,
  }];
}));
const summary = {
  format: 'r1-frozen-snapshot/1',
  method: 'brief v3 §12 E1 steps 1-7: every admitted Claude Code and Codex source of every driver (B0 `sources --json`), '
    + 'physical#member grouped under its physical file and copied once; imgdefer/make_snapshot.js copyStableAndVerify '
    + `(stable stat, SHA-256 verify, ms mtime) with up to ${MAX_ATTEMPTS} attempts 1 s apart; SQLite via .backup + `
    + 'PRAGMA journal_mode=DELETE (integrity ok, readback delete, no sidecars); directories 0555, files 0444',
  sqliteMethod: 'db and present -wal/-shm staged byte-for-byte (group-stable) into a private dir, `.backup` read from the staged '
    + 'copy (sqlite never opens the live source), then journal_mode=DELETE on the copy',
  machine: { model: spawnSync('sysctl', ['-n', 'hw.model'], { encoding: 'utf8' }).stdout.trim(), cpus: os.cpus().length,
    memGb: Math.round(os.totalmem() / 2 ** 30) },
  createdAt: new Date().toISOString(),
  inventoryBinary: { commit: '3ca74109dfd2db11681d8ae9001877191660c8c4', sha256: lib.sha256File(binary),
    note: 'Task B release build (CARGO_INCREMENTAL=0), used for inventory, admission and dry run only' },
  privatePathsRecorded: false,
  agents: byAgent,
  totals: { files: frozen.length, bytes: frozen.reduce((s, f) => s + f.bytes, 0) },
  inventory: {
    sourceEvents: inventory.length,
    memberEvents,
    physicalFiles: physical.size,
    sqliteSidecarsExcluded: sidecarsExcluded,
    grewSinceInventory,
    attemptsHistogram,
    inventoryWallMs,
    copyHashVerifyWallMs: copyWallMs,
  },
  sqlite: sqliteReports,
  admission: {
    ok: true,
    rule: 'snapshot `sources` multiset of (agent, relativePath, stream, driver) equals the inventory minus sqlite sidecars; '
      + 'bytes equal the frozen copy for every source',
    expected: [...expected.values()].reduce((a, b) => a + b, 0),
    admitted: admitted.length,
    sidecarsExcluded,
    modifiedMsMismatches: mtimeMismatch,
    admissionWallMs,
  },
  dryRun,
  ageBucketsFromNewest: ageBuckets(frozen),
  newestSourceAgeNote: 'buckets are non-cumulative, measured back from the newest frozen mtime',
  top10Files: [...frozen].sort((l, r) => r.bytes - l.bytes).slice(0, 10).map((f, i) => ({
    rank: i + 1, agent: f.agent, stream: f.stream, driver: f.driver, bytes: f.bytes, ageHoursFromNewest: hoursBetween(newest, f.mtimeMs) })),
  top10TreesPathProxy: treeList.filter((t) => !t.shared).sort((l, r) => r.bytes - l.bytes).slice(0, 10).map((t, i) => ({
    rank: i + 1, agent: t.agent, files: t.files, bytes: t.bytes, maxSourceBytes: t.maxSourceBytes,
    ageHoursFromNewest: hoursBetween(newest, t.mtimeMs) })),
  treesPathProxyNote: 'path proxy; the TreeMap from metadata links is recorded in snapshot-structure.json by E1.8 (B2 walk-plan)',
  manifestFile: 'SHA256SUMS',
  manifestSha256: createHash('sha256').update(manifestText).digest('hex'),
  walkInputsDigest: walk.digest,
  walkInputsMethod: lib.WALK_INPUTS_METHOD,
  walkInputs: walk.perAgent,
  structure: 'pending E1.8: snapshot-structure.json from one B2 --json --profile run (walk-plan events + census lines)',
};
fs.writeFileSync(path.join(outDir, 'SHA256SUMS'), manifestText, { flag: 'wx', mode: 0o444 });
fs.writeFileSync(path.join(outDir, 'snapshot.json'), `${JSON.stringify(summary, null, 2)}\n`, { flag: 'wx', mode: 0o444 });

// ---------------------------------------------------------------- step 7b
fs.chmodSync(outDir, 0o555);
console.log(JSON.stringify(summary, null, 2));
