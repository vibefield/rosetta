#!/usr/bin/env node
'use strict';
// P3 reader load (brief v3 §12 E3), forked by run_r1.js --load once the
// daemon's `ready` event names its address. Through capture, finalize and
// publication it:
//   - polls /api/sessions every 250 ms;
//   - for each newly non-Pending top-level session, up to 10, GETs its
//     timeline, collaboration, /api/activity (narrowed to it, the 7 d up to
//     its last update) and one full command (the newest shell tool row on its
//     newest timeline page);
//   - keeps /ws/sessions/{id} open for 3 of them;
//   - GETs /api/artifacts every second.
// Every request is stamped with the run phase run_r1.js sends over IPC
// (meta | streaming | finalize | post), at its start. Records status counts
// and latency distributions per (route, phase), 5xx, transport errors, and
// non-2xx bodies naming a lock (SQLITE_LOCKED) or not_ready. No ids, paths
// or content are written: counts and timings only.
//
// usage (normally via run_r1.js): node r1_reader_load.js ORIGIN OUT_JSON

const fs = require('node:fs');
const http = require('node:http');

const [origin, outPath] = process.argv.slice(2);
if (!origin || !outPath) { console.error('usage: r1_reader_load.js ORIGIN OUT_JSON'); process.exit(2); }
const base = new URL(origin);
const agent = new http.Agent({ keepAlive: true, maxSockets: 8 });
const t0 = process.hrtime.bigint();
const now = () => Number(process.hrtime.bigint() - t0) / 1e6;
let phase = 'meta';
const phaseLog = [];
let stopping = false;

const stats = new Map(); // `${route}\t${phase}` -> { lat: [], status: {} }
// http5xx splits by the protocol error code: a 503 `not_ready` is the API's
// designed "still importing" answer, every other 5xx is a server fault. Both
// are reported (P3's "5xx = 0" gate: which count is the reviewer's call).
const totals = { requests: 0, http5xx: 0, http5xxNotReady: 0, http5xxOther: 0, errorCodes: {}, transportErrors: 0,
  transportErrorsByPhase: {}, lockedBodies: 0, notReadyBodies: 0, non2xx: 0 };
const ws = { opened: 0, messages: 0, bytes: 0, errors: 0, errorsByPhase: {}, closes: {}, closesByPhase: {}, closedByServer: 0 };
const sessions = { pollsOk: 0, pollsFailed: 0, firstNonPendingMs: null, nonPendingMax: 0, chosen: 0, commandsFetched: 0, commandsAbsent: 0 };

function record(route, ph, status, ms, body) {
  const key = `${route}\t${ph}`;
  const s = stats.get(key) || { lat: [], status: {} };
  s.lat.push(ms);
  s.status[status] = (s.status[status] || 0) + 1;
  stats.set(key, s);
  totals.requests += 1;
  if (status === 'error') {
    // After the daemon exits (phase post) refused connections are expected.
    totals.transportErrors += 1;
    totals.transportErrorsByPhase[ph] = (totals.transportErrorsByPhase[ph] || 0) + 1;
    return;
  }
  if (status < 200 || status >= 300) {
    let code = 'unparsed';
    try { code = JSON.parse(body).error.code || 'none'; } catch { /* not a protocol error body */ }
    const k = `${route}:${status}:${code}:${ph}`;
    totals.errorCodes[k] = (totals.errorCodes[k] || 0) + 1;
    if (status >= 500) {
      totals.http5xx += 1;
      if (code === 'not_ready') totals.http5xxNotReady += 1;
      else totals.http5xxOther += 1;
    }
    totals.non2xx += 1;
    if (/locked/i.test(body)) totals.lockedBodies += 1;
    if (/not_ready/i.test(body)) totals.notReadyBodies += 1;
  }
}

function get(route, pathAndQuery) {
  const ph = phase;
  const start = now();
  return new Promise((resolve) => {
    const req = http.get({ host: base.hostname, port: base.port, path: pathAndQuery, agent, timeout: 120_000 }, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => {
        const body = Buffer.concat(chunks).toString('utf8');
        record(route, ph, res.statusCode, now() - start, res.statusCode >= 300 ? body.slice(0, 2000) : '');
        resolve({ status: res.statusCode, body });
      });
      res.on('error', () => { record(route, ph, 'error', now() - start, ''); resolve({ status: 0, body: '' }); });
    });
    req.on('timeout', () => req.destroy(new Error('timeout')));
    req.on('error', () => { record(route, ph, 'error', now() - start, ''); resolve({ status: 0, body: '' }); });
  });
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const json = (body) => { try { return JSON.parse(body); } catch { return null; } };

const chosen = new Set();
const sockets = [];

function openSocket(id) {
  let socket;
  try {
    socket = new WebSocket(`ws://${base.host}/ws/sessions/${encodeURIComponent(id)}`);
  } catch { ws.errors += 1; return; }
  sockets.push(socket);
  socket.addEventListener('open', () => { ws.opened += 1; });
  socket.addEventListener('message', (m) => {
    ws.messages += 1;
    ws.bytes += typeof m.data === 'string' ? Buffer.byteLength(m.data) : (m.data.byteLength || m.data.size || 0);
  });
  // Phase-stamped: a 1006 close in phase post is the daemon exiting (--ticks 0).
  socket.addEventListener('error', () => { ws.errors += 1; ws.errorsByPhase[phase] = (ws.errorsByPhase[phase] || 0) + 1; });
  socket.addEventListener('close', (c) => {
    ws.closes[c.code] = (ws.closes[c.code] || 0) + 1;
    const k = `${phase}:${c.code}`;
    ws.closesByPhase[k] = (ws.closesByPhase[k] || 0) + 1;
    if (!stopping) ws.closedByServer += 1;
  });
}

async function readSession(id, updatedAt, withSocket) {
  const enc = encodeURIComponent(id);
  if (withSocket) openSocket(id);
  const tl = await get('timeline', `/api/sessions/${enc}/timeline`);
  await get('collaboration', `/api/sessions/${enc}/collaboration`);
  // The activity window is at most 7 d: the 7 d up to the session's last update.
  const until = (Number(updatedAt) || Date.now()) + 1;
  await get('activity', `/api/activity?sessions=${enc}&since_ms=${until - 7 * 86_400_000 + 1}&until_ms=${until}`);
  const page = tl.status === 200 ? json(tl.body) : null;
  const entries = (page && page.entries) || [];
  const tools = entries.filter((e) => e.body && e.body.type === 'tool_use');
  // A shell tool row (ToolAction::Shell) is the one whose raw command the route serves.
  const command = [...tools].reverse().find((e) => e.body.tool && e.body.tool.action && e.body.tool.action.type === 'shell') || null;
  if (command) {
    sessions.commandsFetched += 1;
    await get('command', `/api/sessions/${enc}/timeline/${encodeURIComponent(command.timeline_id)}/command`);
  } else {
    sessions.commandsAbsent += 1;
  }
}

async function pollSessions() {
  const pending = [];
  while (!stopping) {
    const started = now();
    const r = await get('sessions', '/api/sessions?limit=1000');
    const value = r.status === 200 ? json(r.body) : null;
    if (value && Array.isArray(value.sessions)) {
      sessions.pollsOk += 1;
      const ready = value.sessions.filter((s) => s.stream_state && s.stream_state !== 'pending');
      sessions.nonPendingMax = Math.max(sessions.nonPendingMax, ready.length);
      if (ready.length && sessions.firstNonPendingMs === null) sessions.firstNonPendingMs = Math.round(now());
      for (const s of ready) {
        if (chosen.size >= 10) break;
        if (chosen.has(s.id)) continue;
        chosen.add(s.id);
        sessions.chosen = chosen.size;
        pending.push(readSession(s.id, s.updated_at, chosen.size <= 3));
      }
    } else {
      sessions.pollsFailed += 1;
    }
    await sleep(Math.max(0, 250 - (now() - started)));
  }
  await Promise.all(pending);
}

async function pollArtifacts() {
  while (!stopping) {
    const started = now();
    await get('artifacts', '/api/artifacts');
    await sleep(Math.max(0, 1000 - (now() - started)));
  }
}

function pct(sorted, p) {
  if (!sorted.length) return null;
  return sorted[Math.max(1, Math.ceil((p / 100) * sorted.length)) - 1];
}
const r1 = (v) => (v === null ? null : Math.round(v * 10) / 10);

function finish() {
  const routes = {};
  for (const [key, s] of stats) {
    const [route, ph] = key.split('\t');
    const lat = [...s.lat].sort((a, b) => a - b);
    routes[route] = routes[route] || {};
    routes[route][ph] = { count: lat.length, status: s.status, p50Ms: r1(pct(lat, 50)), p99Ms: r1(pct(lat, 99)), maxMs: r1(lat[lat.length - 1]) };
  }
  const report = { format: 'r1-reader-load/1', origin: 'loopback', wallMs: Math.round(now()), phases: phaseLog, totals, sessions, websockets: ws, routes };
  fs.writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 });
}

process.on('message', async (m) => {
  if (m.type === 'phase') {
    phase = m.phase;
    phaseLog.push({ phase: m.phase, readerMs: Math.round(now()), runMs: Math.round(m.ms) });
  } else if (m.type === 'stop') {
    stopping = true;
    for (const s of sockets) { try { s.close(); } catch { /* closed */ } }
    await Promise.race([Promise.all([sessionsLoop, artifactsLoop]), sleep(10_000)]);
    finish();
    agent.destroy();
    process.exit(0);
  }
});
process.on('disconnect', () => { if (!stopping) { stopping = true; finish(); process.exit(0); } });

const sessionsLoop = pollSessions();
const artifactsLoop = pollArtifacts();
