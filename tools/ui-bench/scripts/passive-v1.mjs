// passive-v1: consent, then only observe. The diagnostic baseline beside reader-v1:
// nothing is selected and the pointer rests on the transcript column once the
// columns are up, so no list is held by hover or focus. Versioned like reader-v1;
// its sha256 is recorded in each run.
const ENROLL = ['claude-code', 'codex'];
// What this script never exercises (metrics.mjs `reached` reads these as n/a): it
// selects nothing, opens nothing, aims at nothing and never searches. A `timeout-*`
// mark makes the run incomplete.
export const NOT_APPLICABLE = ['H2.ms', 'H3.h24.paintedSelMs', 'H3.h7d.paintedSelMs', 'Hall.paintedSelMs', 'H4.samples', 'H4.pendingFace',
  'H7.aims', 'H8.chartsSelectedMs', 'H8.searchLagMs'];

async function consent(ctx) {
  if (!(await ctx.until('rollcall-ready', { timeoutMs: 120_000 }))) throw new Error('the roll call never drew its rows');
  for (const row of (await ctx.q('rollcall')) ?? []) {
    const want = ENROLL.includes(row.kind);
    if (row.checked !== want && !row.disabled) await ctx.aim(`kind:${row.kind}`);
  }
  const rows = (await ctx.q('rollcall')) ?? [];
  const exact = rows.every((r) => r.checked === ENROLL.includes(r.kind)) && ENROLL.every((k) => rows.some((r) => r.kind === k && r.checked));
  ctx.mark(exact ? 'enrolled-ok' : 'enrolled-mismatch');
  if (!exact) throw new Error('the roll call does not offer exactly the enrolled kinds');
  if (!(await ctx.aim('begin')).ok) throw new Error('no Begin button');
  ctx.mark('consent-begin');
}

export default {
  name: 'passive-v1',
  async run(ctx) {
    await consent(ctx);
    if (!(await ctx.until('cols-up', { timeoutMs: 120_000 }))) { ctx.mark('timeout-cols'); return; }
    if ((await ctx.aim('park', { click: false })).ok) ctx.mark('park');
    if (!(await ctx.until('status-search', { timeoutMs: 600_000 }))) ctx.mark('timeout-search-ready');
    for (const name of ['charts-all', 'portfolio']) {
      if (!(await ctx.until(name, { timeoutMs: 60_000 }))) ctx.mark('timeout-charts');
    }
    ctx.mark('done');
  },
};
