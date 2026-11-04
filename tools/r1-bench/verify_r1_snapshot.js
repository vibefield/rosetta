#!/usr/bin/env node
'use strict';
// Before every run (pair_r1.sh): the frozen snapshot is exactly what
// snapshot.json describes. Checks, binary-free:
//   - walkInputsDigest recomputed from the tree equals snapshot.json's;
//   - SHA256SUMS: its own SHA-256 equals manifestSha256, every listed file
//     hashes to its line, and no file under home/ is unlisted;
//   - permissions: every file 0444, every directory 0555 (snapshot root too);
//   - no SQLite sidecar (-wal/-shm/-journal) anywhere.
// Prints one JSON line (counts, digests, wall) and exits 1 on any mismatch.
// Content hashing reads ~1.8 GB, so every run starts from the same warm page
// cache on both sides.
//
// usage: node verify_r1_snapshot.js SNAPSHOT_DIR [--no-content]

const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const lib = require('./r1_snapshot_lib.js');

const [snap, flag] = process.argv.slice(2);
if (!snap) { console.error('usage: verify_r1_snapshot.js SNAPSHOT_DIR [--no-content]'); process.exit(2); }
const t0 = process.hrtime.bigint();
const problems = [];
const meta = JSON.parse(fs.readFileSync(path.join(snap, 'snapshot.json'), 'utf8'));
const sumsText = fs.readFileSync(path.join(snap, 'SHA256SUMS'), 'utf8');
const home = path.join(snap, 'home');

const manifestSha = createHash('sha256').update(sumsText).digest('hex');
if (manifestSha !== meta.manifestSha256) problems.push('SHA256SUMS differs from manifestSha256');

const roots = Object.entries(meta.agents).map(([agent, v]) => ({ agent, dataRoot: v.dataRoot }));
const walk = lib.walkInputs(home, roots);
if (walk.digest !== meta.walkInputsDigest) problems.push('walkInputsDigest mismatch');

const listed = new Map();
for (const line of sumsText.split('\n').filter(Boolean)) {
  const m = line.match(/^([0-9a-f]{64}) {2}(.+)$/);
  if (!m) { problems.push('malformed SHA256SUMS line'); continue; }
  listed.set(m[2], m[1]);
}
const onDisk = walk.entries.map((e) => path.relative(snap, e.abs).split(path.sep).join('/'));
const unlisted = onDisk.filter((p) => !listed.has(p)).length;
const absent = [...listed.keys()].filter((p) => !onDisk.includes(p)).length;
if (unlisted) problems.push(`${unlisted} files not in SHA256SUMS`);
if (absent) problems.push(`${absent} SHA256SUMS entries missing`);
const badFileMode = walk.entries.filter((e) => e.mode !== 0o444).length;
if (badFileMode) problems.push(`${badFileMode} files not 0444`);
const dirs = lib.directoriesBelow(snap);
const badDirMode = dirs.filter((d) => (fs.statSync(d).mode & 0o777) !== 0o555).length;
if (badDirMode) problems.push(`${badDirMode} directories not 0555`);
const sidecars = walk.entries.filter((e) => /\.sqlite-(wal|shm|journal)$|\.db-(wal|shm|journal)$/.test(e.rel)).length;
if (sidecars) problems.push(`${sidecars} sqlite sidecars present`);
for (const f of ['SHA256SUMS', 'snapshot.json']) {
  if ((fs.statSync(path.join(snap, f)).mode & 0o777) !== 0o444) problems.push(`${f} not 0444`);
}

let hashed = 0;
let hashedBytes = 0;
let contentMismatch = 0;
if (flag !== '--no-content') {
  for (const e of walk.entries) {
    const rel = path.relative(snap, e.abs).split(path.sep).join('/');
    const want = listed.get(rel);
    if (!want) continue;
    if (lib.sha256File(e.abs) !== want) contentMismatch += 1;
    hashed += 1;
    hashedBytes += e.size;
  }
  if (contentMismatch) problems.push(`${contentMismatch} files differ from SHA256SUMS`);
}

const out = {
  ok: problems.length === 0,
  problems,
  walkInputsDigest: walk.digest,
  manifestSha256: manifestSha,
  files: walk.entries.length,
  bytes: walk.entries.reduce((s, e) => s + e.size, 0),
  hashedFiles: hashed,
  hashedBytes,
  directories: dirs.length,
  wallMs: Math.round(Number(process.hrtime.bigint() - t0) / 1e6),
};
console.log(JSON.stringify(out));
process.exit(out.ok ? 0 : 1);
