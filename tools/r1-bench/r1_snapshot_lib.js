'use strict';
// Shared by make_r1_snapshot.js (writes the manifest) and verify_r1_snapshot.js
// (pair_r1.sh runs it before every run). Binary-free: it reads the frozen
// tree directly, so a verification never depends on the binary under test.
//
// walkInputsDigest (brief v3 §12 E1 step 6, L minor 14): SHA-256 over
// (agent, path rank, size, mtime_ms) of every admitted source, in path order.
// Path order = the source's path relative to its agent's data root, compared
// as UTF-8 bytes (Rust String order); rank is 1-based within the agent.
// Agents in byte order of their kind. One line per source:
//   `${agent}\t${rank}\t${size}\t${mtime_ms}\n`
// Paths themselves are not hashed: SHA256SUMS pins names and content; this
// digest pins what the walk planner reads (cut_len / cut_modified_ms, else the
// census size and mtime), which SHA256SUMS cannot see.

const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');

const WALK_INPUTS_METHOD = 'sha256 over lines "agent\\trank\\tsize\\tmtime_ms\\n", agents in byte order, '
  + 'sources in UTF-8 byte order of their data-root-relative path, rank 1-based per agent, '
  + 'mtime_ms = floor(mtime_ns / 1e6); every regular file under each agent data root of the frozen home';

function byteCompare(l, r) {
  return Buffer.compare(Buffer.from(l, 'utf8'), Buffer.from(r, 'utf8'));
}

function filesBelow(root) {
  const out = [];
  (function visit(dir, rel) {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const abs = path.join(dir, e.name);
      const r = rel ? `${rel}/${e.name}` : e.name;
      if (e.isDirectory()) visit(abs, r);
      else if (e.isFile()) out.push({ rel: r, abs });
      else throw new Error('snapshot holds a non-regular, non-directory entry');
    }
  })(root, '');
  return out;
}

function expandDataRoot(dataRoot, home) {
  if (dataRoot === '~') return home;
  if (dataRoot.startsWith('~/')) return path.join(home, dataRoot.slice(2));
  throw new Error('the frozen snapshot requires a home-relative data root');
}

// roots: [{ agent, dataRoot }]. Returns { digest, perAgent: {agent: {files, bytes}}, entries }.
function walkInputs(home, roots) {
  const agents = [...roots].sort((l, r) => byteCompare(l.agent, r.agent));
  const hash = createHash('sha256');
  const perAgent = {};
  const entries = [];
  for (const { agent, dataRoot } of agents) {
    const root = expandDataRoot(dataRoot, home);
    const files = filesBelow(root).sort((l, r) => byteCompare(l.rel, r.rel));
    let bytes = 0;
    files.forEach((f, i) => {
      const st = fs.statSync(f.abs, { bigint: true });
      const size = Number(st.size);
      const mtimeMs = Number(st.mtimeNs / 1_000_000n);
      bytes += size;
      hash.update(`${agent}\t${i + 1}\t${size}\t${mtimeMs}\n`);
      entries.push({ agent, rank: i + 1, rel: f.rel, abs: f.abs, size, mtimeMs, mode: Number(st.mode) & 0o777 });
    });
    perAgent[agent] = { files: files.length, bytes };
  }
  return { digest: hash.digest('hex'), perAgent, entries };
}

function sha256File(file) {
  const hash = createHash('sha256');
  const fd = fs.openSync(file, 'r');
  const buffer = Buffer.allocUnsafe(4 * 1024 * 1024);
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

function directoriesBelow(root) {
  const out = [];
  (function visit(d) {
    out.push(d);
    for (const e of fs.readdirSync(d, { withFileTypes: true })) if (e.isDirectory()) visit(path.join(d, e.name));
  })(root);
  return out;
}

module.exports = { WALK_INPUTS_METHOD, byteCompare, filesBelow, expandDataRoot, walkInputs, sha256File, directoriesBelow };
