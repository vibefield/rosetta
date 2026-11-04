// reader-v1: the scripted reader the gates use. Versioned and event-triggered: every
// step waits on an import event the probe saw or a page state, never on a wall-clock
// aim. Its sha256 is recorded in each run; a changed script is a new version, and runs
// of different versions are never compared. Keyboard focus stays where a click leaves
// it, as for a real reader, so the lists hold their geometry the way they do for one.
//
//   1. consent: exactly claude-code and codex, then Begin;
//   2. the columns are up and NEWEST is frozen: select NEWEST's project, and see that
//      it is selected: a press the list's reorder sent to another card (the card moved
//      before the pointer reached the list, so nothing held it; H7 records the miss) is
//      marked and aimed again once the card is at rest, at most 3 aims in all, as a
//      reader would click again;
//   3. NEWEST reads ready: 300 ms later, open it (H4); if the import's capture ends
//      first (all readable) and NEWEST is still not drawn ready within 5 s, it was
//      never rostered and the step is skipped (marked, not a timeout);
//   4. a row still not imported: open it once (the opened-while-Pending face);
//   5. each of the next 5 flips in that project: open the next ready 24 h member
//      (H4, H7 c), as the design's "up to 5 RECENT24 members";
//   6. each horizon reached (24 h, 7 d), or all readable if a horizon has a
//      never-rostered member: aim at the next project card, then back;
//   7. all readable: open a ready row; canonical readiness: open another (H4 by phase);
//   8. search ready: ⌘K, three fixed queries (text only here), each settled and
//      digested by the probe, then Esc (H8 b, UI hit digest; the guard is timed from
//      the later of the UI's search-ready receipt and the last key, so this
//      script's queue never enters it);
//   9. charts: the selected panel, every card, the portfolio (H8 a).
// A `timeout-*` mark makes the run incomplete. The queries are product-diagnostic-v3
// cases broad-1, medium-1 and medium-2 (tools/r1-bench/product-diagnostic-v3.json);
// only their index is ever exported.
export const QUERIES = ['error', 'sqlite', 'cargo'];
// Every metric value this script exercises (metrics.mjs `reached`): none is n/a.
export const NOT_APPLICABLE = [];
const ENROLL = ['claude-code', 'codex'];

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

async function open(ctx, spec, name, waitFor = 'content') {
  const before = ctx.count(waitFor);
  const aimed = await ctx.aim(spec);
  if (!aimed.ok) return false;
  ctx.mark(name);
  if (!(await ctx.until(waitFor, { count: before + 1, timeoutMs: 15_000 }))) ctx.mark('timeout-open');
  return true;
}

async function untilState(ctx, test, timeoutMs) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    const s = await ctx.q('state');
    if (s && test(s)) return s;
    await ctx.sleep(50);
  }
  return null;
}

/** Step 2: aim at NEWEST's project card until the page says it is selected. The
 * probe judges each press (it met the aimed card, or another); after a miss the
 * reader looks again once the card is at rest (two reads 50 ms apart agree), so a
 * card still sliding from the reorder is not aimed at where it was. */
async function selectNewestProject(ctx) {
  for (let tries = 0; tries < 3; tries += 1) {
    if (tries > 0) {
      let last = null;
      for (let i = 0; i < 20; i += 1) {
        const r = await ctx.q('rect', 'newest-project');
        if (r && last && Math.abs(r.y - last.y) < 1) break;
        last = r;
        await ctx.sleep(50);
      }
    }
    const aimed = await ctx.aim('newest-project');
    if (!aimed.ok) return;
    let hit = null;
    for (let i = 0; i < 10 && hit === null; i += 1) {
      hit = await ctx.q('aimHit', aimed.aimId);
      if (hit === null) await ctx.sleep(50);
    }
    if (hit === true && (await untilState(ctx, (s) => s.newestSelected, 1_000))) { ctx.mark('select-newest-project'); return; }
    ctx.mark('select-newest-project-missed');
  }
}

export default {
  name: 'reader-v1',
  async run(ctx) {
    await consent(ctx);
    if (!(await ctx.until('cols-up', { timeoutMs: 120_000 }))) { ctx.mark('timeout-cols'); return; }
    if (!(await ctx.until('frozen', { timeoutMs: 60_000 }))) { ctx.mark('timeout-frozen'); return; }
    await selectNewestProject(ctx);

    let ready = await ctx.untilAny([['newest-ready', 1], ['all-readable', 1]], 300_000);
    if (ready === 'all-readable') ready = (await ctx.until('newest-ready', { timeoutMs: 5_000 })) ? 'newest-ready' : 'never';
    if (ready === 'newest-ready') {
      await ctx.sleep(300);
      await open(ctx, 'newest-row', 'open-newest');
    } else ctx.mark(ready === 'never' ? 'newest-never-ready' : 'timeout-newest-ready');

    const now = await ctx.q('state');
    if (now && now.pendingRows > 0) await open(ctx, 'pending-row', 'open-pending', 'pending-face');

    const base = ctx.count('flip-sel');
    for (let k = 1; k <= 5; k += 1) {
      const got = await ctx.untilAny([['flip-sel', base + k], ['all-readable', 1]], 300_000);
      if (got !== 'flip-sel') break;
      if (!(await open(ctx, 'recent-next', 'open-recent'))) await open(ctx, 'ready-next', 'open-recent');
    }

    for (const horizon of ['h24', 'h7d']) {
      const got = await ctx.untilAny([[horizon, 1], ['all-readable', 1]], 300_000);
      if (got === null) { ctx.mark('timeout-horizon'); continue; }
      if (got !== horizon) ctx.mark('horizon-via-all-readable');
      if ((await ctx.aim('other-project:1')).ok) ctx.mark('aim-horizon');
      if ((await ctx.aim('newest-project')).ok) ctx.mark('reselect');
    }

    if (await ctx.until('all-readable', { timeoutMs: 300_000 })) await open(ctx, 'ready-next', 'open-finalizing');
    else ctx.mark('timeout-all-readable');
    if (await ctx.until('status-canonical', { timeoutMs: 600_000 })) await open(ctx, 'ready-next', 'open-post-canonical');
    else ctx.mark('timeout-canonical');

    if (await ctx.until('status-search', { timeoutMs: 600_000 })) {
      await ctx.key('k', { meta: true });
      if (await untilState(ctx, (s) => s.sheetOpen, 5_000)) {
        ctx.mark('search-open');
        let typed = 0;
        for (let i = 0; i < QUERIES.length; i += 1) {
          for (let j = 0; j < typed; j += 1) await ctx.key('Backspace');
          await ctx.q('searchBegin', i);
          await ctx.type(QUERIES[i], 10);
          await ctx.q('searchTyped', i);
          ctx.mark('search-query');
          if (!(await ctx.until('sheet-quiet', { count: i + 1, timeoutMs: 30_000 }))) ctx.mark('timeout-search');
          typed = QUERIES[i].length;
        }
        await ctx.key('Escape');
        ctx.mark('search-close');
      } else ctx.mark('timeout-sheet');
    } else ctx.mark('timeout-search-ready');

    for (const name of ['charts-selected', 'charts-all', 'portfolio']) {
      if (!(await ctx.until(name, { timeoutMs: 60_000 }))) ctx.mark('timeout-charts');
    }
    ctx.mark('done');
  },
};
