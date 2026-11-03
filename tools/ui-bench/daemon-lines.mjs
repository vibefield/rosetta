// Parse the drained daemon stdout (`<receipt epoch ms>\t<line>` per line, drain.mjs)
// into the few facts the UI metrics pair with: the clock anchor, the milestones, the
// tree-admission batches and the memory peak. Pure; never returns a path, a runtime
// id or anything else a line may carry beyond names, counts and seconds.

// Milestone keys the run JSON may carry (privacy schema enum). Anything else is counted.
export const MILESTONE_KEYS = [
  'consent-granted', 'metadata-ready', 'first-session-complete', 'first-readable',
  'recent-ready:24h', 'recent-ready:7d', 'recent-ready:24h@newest', 'recent-ready:7d@newest',
  'newest-trees-ready', 'newest-giant-ready', 'small-recent-ready:24h@newest', 'small-recent-ready:7d@newest',
  'runtime-metadata-public:claude-code', 'runtime-metadata-public:codex',
  'runtime-readable:claude-code', 'runtime-readable:codex',
  'canonical-ready', 'memory-peak', 'search-ready', 'search-failed',
];
const KNOWN = new Set(MILESTONE_KEYS);

function fields(rest) {
  const out = {};
  for (const token of rest.trim().split(/\s+/)) {
    const eq = token.indexOf('=');
    if (eq > 0) out[token.slice(0, eq)] = token.slice(eq + 1);
  }
  return out;
}
const num = (v) => (v === undefined || v === 'na' || v === '' ? null : Number(v));

export function parseDaemonLog(text) {
  const out = {
    clockWallMs: null, clockReceiptAt: null, readyReceiptAt: null, lines: 0, jsonEvents: 0,
    milestones: {}, unknownMilestones: 0, receiptLagMs: [],
    treeAdmissions: [], memoryPeak: null, runtimeReadableSessions: 0,
    // Σ `<runtime>: metadata public — N sessions`: every session the skims made
    // public, the daemon's own count beside the roster's (status sessions_total).
    metadataPublicSessions: null, metadataPublicLines: 0,
    panicLines: 0,
  };
  const precise = {};
  // Receipt minus the daemon's own stamp, over JSON events whose stamp is unrounded
  // (milestone twins' elapsedS, tree-admission publishedS): the clock cross-check.
  const preciseStamps = [];
  for (const raw of text.split('\n')) {
    if (!raw) continue;
    const tab = raw.indexOf('\t');
    if (tab < 0) continue;
    const receipt = Number(raw.slice(0, tab));
    const line = raw.slice(tab + 1);
    out.lines += 1;
    if (line.startsWith('{')) {
      out.jsonEvents += 1;
      let ev;
      try { ev = JSON.parse(line); } catch { continue; }
      if (ev.event === 'ready' && out.readyReceiptAt === null) out.readyReceiptAt = receipt;
      else if (ev.event === 'clock' && out.clockWallMs === null) { out.clockWallMs = Number(ev.wallMs); out.clockReceiptAt = receipt; }
      else if (ev.event === 'milestone' && typeof ev.milestone === 'string') {
        // The JSON twin of a milestone line carries elapsedS unrounded (the text line has
        // two decimals); it refines the line's value below.
        const key = ev.horizon ? `${ev.milestone}:${ev.horizon}` : ev.milestone;
        if (precise[key] === undefined) precise[key] = Number(ev.elapsedS);
        if (Number.isFinite(Number(ev.elapsedS))) preciseStamps.push([receipt, Number(ev.elapsedS)]);
      }
      else if (ev.event === 'stage-profile' && ev.stage === 'tree-admission') {
        out.treeAdmissions.push({ elapsedS: num(ev.elapsedS), publishedS: num(ev.publishedS), trees: num(ev.trees),
          sessions: num(ev.sessions), roots: num(ev.roots), wallMs: num(ev.wallMs), lockHeldMs: num(ev.lockHeldMs) });
        if (Number.isFinite(num(ev.publishedS))) preciseStamps.push([receipt, num(ev.publishedS)]);
      }
      continue;
    }
    if (/panicked at/.test(line)) out.panicLines += 1;
    const pub = line.match(/: metadata public — (\d+) sessions,/);
    if (pub) { out.metadataPublicSessions = (out.metadataPublicSessions ?? 0) + Number(pub[1]); out.metadataPublicLines += 1; continue; }
    const clock = line.match(/^clock: wall_ms=(\d+)/);
    if (clock) { if (out.clockWallMs === null) { out.clockWallMs = Number(clock[1]); out.clockReceiptAt = receipt; } continue; }
    const m = line.match(/^milestone: (\S+)(.*)$/);
    if (!m) continue;
    const name = m[1];
    const f = fields(m[2]);
    const elapsedS = num(f.elapsed_s);
    let key = name;
    if (f.horizon) key = `${name}:${f.horizon}`;
    else if ((name === 'runtime-readable' || name === 'runtime-metadata-public') && f.agent) key = `${name}:${f.agent}`;
    if (name === 'runtime-readable') out.runtimeReadableSessions += num(f.sessions) ?? 0;
    if (name === 'memory-peak') {
      out.memoryPeak = { footprintMb: num(f.phys_footprint_mb), residentMb: num(f.resident_mb), swapUsedMb: num(f.swap_used_mb),
        swapouts: num(f.swapouts), pageouts: num(f.pageouts) };
    }
    if (!KNOWN.has(key)) { out.unknownMilestones += 1; continue; }
    if (out.milestones[key] !== undefined) continue;
    const rec = { elapsedS, receiptAt: receipt };
    if (f.horizon_sessions !== undefined) rec.horizonSessions = num(f.horizon_sessions);
    if (f.sessions !== undefined) rec.sessions = num(f.sessions);
    if (f.trees !== undefined) rec.trees = num(f.trees);
    if (f.n !== undefined) rec.n = num(f.n);
    if (f.tree_bytes !== undefined) rec.treeBytes = num(f.tree_bytes);
    out.milestones[key] = rec;
  }
  for (const [key, rec] of Object.entries(out.milestones)) {
    if (precise[key] !== undefined && Number.isFinite(precise[key])) { rec.elapsedS = precise[key]; rec.precise = true; } else rec.precise = false;
  }
  if (out.clockWallMs !== null) out.receiptLagMs = preciseStamps.map(([receipt, s]) => receipt - (out.clockWallMs + s * 1000));
  return out;
}

/** The latest runtime-readable milestone: the daemon's "everything readable". */
export function allReadableS(daemon) {
  const xs = Object.entries(daemon.milestones).filter(([k]) => k.startsWith('runtime-readable:')).map(([, v]) => v.elapsedS);
  return xs.length ? Math.max(...xs) : null;
}
