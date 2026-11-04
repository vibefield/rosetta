// Synthetic ui-bench inputs for the tests: a probe export, the Electron main notes
// and a drained daemon stdout, all invented (no snapshot, no live directory). Times
// are chosen on one import clock: the daemon's clock line is W0 and consent-granted
// is at 10 s, so import-clock ms `x` is wall W0 + 10 000 + x on every clock.
export const W0 = 1_800_000_000_000;
export const at = (ms) => W0 + 10_000 + ms;
export const HEX = (c) => c.repeat(64);

/** Daemon stdout as drain.mjs writes it: `<receipt>\t<line>`. */
export function daemonStdout({ consent = true, clock = true, extra = [] } = {}) {
  const lines = [];
  const put = (receiptMs, line) => lines.push(`${(W0 + receiptMs).toFixed(3)}\t${line}`);
  if (clock) { put(1, `clock: wall_ms=${W0}`); put(1.2, `{"event":"clock","wallMs":${W0}}`); }
  put(20, '{"announce":"/x/y.json","event":"ready","origin":"http://127.0.0.1:1","protocol_version":29}');
  const ms = (s, text) => put(s * 1000 + 0.4, `milestone: ${text} elapsed_s=${s.toFixed(2)}`);
  if (consent) {
    ms(10, 'consent-granted');
    put(10_000.5, '{"elapsedS":10,"event":"milestone","milestone":"consent-granted"}');
  }
  put(10_040, 'claude-code: metadata public — 10 sessions, 3 projects, 10 streamable files');
  put(10_045, 'codex «Kim»: metadata public — 2 sessions, 1 projects, 2 streamable files');
  ms(10.05, 'metadata-ready');
  ms(10.06, 'runtime-metadata-public runtime=abc agent=claude-code');
  ms(10.1, 'first-readable');
  ms(10.2, 'runtime-readable runtime=def agent=codex sessions=2');
  ms(11, 'recent-ready horizon=24h@newest horizon_sessions=6');
  put(11_000.6, '{"elapsedS":11.0004,"event":"milestone","horizon":"24h@newest","horizonSessions":6,"milestone":"recent-ready"}');
  ms(12, 'recent-ready horizon=7d@newest horizon_sessions=9');
  ms(12.4, 'newest-giant-ready tree_bytes=900000000');
  ms(12.5, 'runtime-readable runtime=abc agent=claude-code sessions=10');
  ms(13, 'canonical-ready');
  put(13_000.5, 'milestone: memory-peak phys_footprint_mb=800 resident_mb=900 swap_used_mb=na swapouts=0 pageouts=3 elapsed_s=13.00');
  ms(13.2, 'search-ready');
  ms(13.3, 'something-new');
  for (const [e, p, roots, sessions] of [[10.099, 10.1, 2, 4], [11, 11.0005, 1, 3], [12.5, 12.5002, 3, 5]]) {
    put(p * 1000 + 0.2, JSON.stringify({ event: 'stage-profile', stage: 'tree-admission', elapsedS: e, publishedS: p, roots, sessions, trees: roots, wallMs: 1.5, lockHeldMs: 0.7 }));
  }
  put(14_000, 'thread \'x\' panicked at nowhere');
  return [...lines, ...extra].join('\n') + '\n';
}

/** A probe export shaped like probe.js's `finish()` result. */
export function probeLog(over = {}) {
  const base = {
    format: 'ui-probe/1',
    selfTest: { firstMetaAt: at(-3000), wsWrapped: true, fetchWrapped: true, worker: 'worker', loaf: true, eventTiming: true, layoutShift: true },
    page: { firstFrameAt: at(-5000), firstRollCallAt: at(-4900), rollCallReadyAt: at(-4800), dpr: 2, innerW: 1500, innerH: 912, focusedAtStart: false, focusedAtRollCall: true },
    faces: [[at(-5000), 'connecting'], [at(-4900), 'roll-call-ready'], [at(-2), 'roll-call-starting'], [at(2050), 'cols']],
    states: [[at(-5000), 'none'], [at(2050), 'reading'], [at(2600), 'finalizing'], [at(3150), 'complete'], [at(8300), 'none']],
    readyCount: [[at(2050), 4], [at(2600), 12]],
    readyRoots: [[at(100), 1, 6], [at(1000.5), 3, 6], [at(2500.3), 6, 6]],
    notImported: [[at(2400), 1], [at(2520), 0]],
    marks: [[at(-2), 'consent-begin'], [at(2300), 'select-newest-project']],
    inputs: [[at(-2), 'pointerdown'], [at(-1.9), 'pointerup'], [at(2700), 'pointerdown'], [at(3500), 'pointerdown']],
    status: { rtt: [[at(1990), at(2000)], [at(2490), at(2500)], [at(3090), at(3100)]], aborted: 0, firstNonConsentAt: at(2000), firstCanonicalAt: at(3100),
      firstSearchReadyAt: at(3300), firstLiveAt: at(3100), metaReadyAtWall: W0 + 10_050, firstPostConsentPhase: 'backfilling',
      last: { phase: 'live', canonical: true, search: 'ready', done: 12, total: 12, run: 'succeeded' }, kinds: ['claude-code', 'codex'], ingestKinds: ['claude-code', 'codex'] },
    meta: { sockets: 1, firstProjectFrameAt: at(60), flipFrames: [at(100), at(1000.5), at(2500.3)], selFlips: [at(2500.3)], retractions: 0 },
    frozen: { at: at(2001), roots: 6, metaOnly: 0, newestKnown: true, projectRoots: 3 },
    h1: { paintAt: at(2050), projects: 3 },
    h2: { firstDrawAt: at(2400), firstDrawStanding: 'ready', indexAtFirstDraw: 0, paintReadyAt: at(2400), receiptAt: at(1000.5), heldReadyMs: 40, newestExcluded: false },
    h3: { h24ReceiptAt: at(1001), h7dReceiptAt: at(2001), n24: 3, n7d: 5, n24Sel: 2, n7dSel: 3, h24PaintSelAt: at(2400), h7dPaintSelAt: at(2600),
      excluded24: 0, excluded7d: 0, excludedSel: 0, h24ViaCaptureOver: false, h7dViaCaptureOver: false },
    hall: { paintSelAt: at(2600) },
    opens: [
      { clickAt: at(2700), standing: 'ready', phase: 'backfilling', canonical: false, contentAt: at(2950), faceAt: null, course: 'none', coverAt: at(2710), coverEndAt: at(2940),
        label: 'loading', revealAt: at(2940), wsAt: at(2705), openAt: at(2708), helloAt: at(2709), snapAt: at(2900), snapBytes: 700000, parseMs: 1.2 },
      { clickAt: at(2800), standing: 'not-imported', phase: 'finalizing', canonical: false, contentAt: null, faceAt: at(2830), course: 'newest-first', coverAt: null,
        coverEndAt: null, label: 'none', revealAt: at(2825), wsAt: at(2802), openAt: at(2804), helloAt: at(2805), snapAt: at(2820), snapBytes: 100, parseMs: 0.1 },
      { clickAt: at(3500), standing: 'ready', phase: 'live', canonical: true, contentAt: at(3560), faceAt: null, course: 'none', coverAt: at(3505), coverEndAt: at(3555),
        label: 'loading', revealAt: at(3555), wsAt: at(3503), openAt: at(3505), helloAt: at(3506), snapAt: at(3530), snapBytes: 60000, parseMs: 0.3 },
    ],
    h5: [[at(2500), at(2520)], [at(2501), at(2541)]],
    loaf: [[at(500), 120, 70], [at(2600), 60, 10], [at(3400), 55, 5], [at(-1000), 300, 250]],
    events: [[at(2700), 48, 'pointerdown', 7, 2], [at(2700), 56, 'click', 7, 3], [at(3500), 24, 'pointerdown', 9, 1]],
    layoutShift: { sum: 0.05, sumNoInput: 0.01, n: 4 },
    h7: { projects: { moves: 2, px: 30, movesDiscrete: 3, pxDiscrete: 45 }, sessions: { moves: 0, px: 0, movesDiscrete: 0, pxDiscrete: 0 }, selectionJumps: 0, selectionJumpsDiscrete: 0,
      aims: [[at(-10), 'begin', 1, 0, 'aimed'], [at(2300), 'newest-project', 1, 0, 'aimed'], [at(2650), 'newest-row', 1, 0, 'aimed']], heldMs: { projects: 120, sessions: 0 }, pillShown: { projects: 1, sessions: 0 } },
    h8: { chartsSelectedAt: at(3600), chartsVisibleAt: at(3550), chartsAllAt: at(3700), portfolioAt: at(8300),
      search: [{ q: 0, beginAt: at(3400), typedAt: at(3450), firstFulltextAt: at(3650), firstTitleAt: null, quietAt: at(4200), rows: 12, fulltextRows: 12, digest: HEX('a') },
        { q: 1, beginAt: at(4300), typedAt: at(4350), firstFulltextAt: at(4500), firstTitleAt: null, quietAt: at(5000), rows: 3, fulltextRows: 3, digest: HEX('b') }] },
    i1: { c1Before: 0, c1PendingReady: 0, c2: 0, c3: 0, c3Discrete: 0, c3ByList: { projects: 0, sessions: 0 }, c3Frames: 40, selectionChanges: 2,
      c4: 0, c4Shown: 0, c4Indexing: 1, c4Opens: 2, c7: 0, c7Faces: 1, checkedRows: 6 },
    d1: { beginAt: at(-2), progressAt: at(2050), titleRowsAfterSearchMs: 0, sheetAfterSearch: true },
    d2: { rank: [[0, at(2050), 4, 2], [5000, at(7050), 0, 0], [30000, null, null, null]], inversionSelMs: 15, selShown: true, giantReceiptAt: at(2401), giantPaintAt: null, giantKnown: true },
    d3: { covers: 2, coversOver100: 1, labels: { loading: 2 }, recovers: 0, errorCovers: 0, sockets: 3, secondSockets: 0, sessionResyncs: 0, wsErrorFrames: 0, http503: 0,
      httpNotOk: 0, longestSnapshotWaitMs: 195 },
    renderedNewestProjectRows: 3,
    cost: { mainMs: 30, postMs: 2, postBytes: 1_000_000, posts: 40, passes: 50, passMs: 25, apiMs: 3, observerCalls: 60, errors: 0 },
    end: { rootsSeen: 6, readyAll: 12, sessionsSeen: 12, pendingSessions: 0, pendingRoots: 0, metaOnlyRoots: 0, neverRostered: 0, lastRootReady: at(2500.3), daemonRoots: 6,
      daemonPending: 0, daemonTruncated: false, projectMismatches: 0, newestProjectRoots: 3, newestProjectDaemonRoots: 3, membershipStable: true,
      inversionMs: 30, inversionFootMs: 45, workerParseMs: 3, frames: 30, snapshots: 1, mutations: 29, resyncs: 0, metaBytes: 40000, statusBodies: 9, retractions: 0 },
    finishedAt: at(9000),
  };
  return deepMerge(base, over);
}

export function mainNotes(over = {}) {
  return deepMerge({
    times: { wrapperStartAt: at(-5300), windowAt: at(-5200), setupDoneAt: at(-5150), didFinishLoadAt: at(-5010), scriptStartAt: at(-4950), scriptEndAt: at(8400) },
    offsets: { start: { offsetMs: 0.1, rttMs: 0.3, n: 20 }, end: { offsetMs: 0.2, rttMs: 0.4, n: 20 } },
    heap: { peakMb: 40, appPeakMb: 600, samples: 14 },
    stubbed: { 'ah-worktree-open': 1, 'ah-worktree-git-facts': 0, 'ah-worktree-file-action': 0, 'ah-favicon': 0, 'ah-file-browsers': 0,
      'ah-file-open-browser-preview': 0, 'shell-open-external': 0, 'network-blocked': 0, 'foreign-binding-calls': 0 },
    inputs: [[at(-2), 'down'], [at(-2), 'up'], [at(2700), 'down'], [at(3500), 'down'], [at(3450), 'key']],
    marks: [[at(-2), 'consent-begin'], [at(2300), 'select-newest-project'], [at(8400), 'done']],
    window: { contentW: 1500, contentH: 912, dpr: 2, displayW: 1920, displayH: 1080 }, eventCounts: { 'first-meta': 1 }, scriptOk: true, scriptError: null,
  }, over);
}

function deepMerge(base, over) {
  if (over === null || typeof over !== 'object' || Array.isArray(over)) return over === undefined ? base : over;
  const out = Array.isArray(base) ? [...base] : { ...base };
  for (const [k, v] of Object.entries(over)) {
    out[k] = v !== null && typeof v === 'object' && !Array.isArray(v) && base && typeof base[k] === 'object' && base[k] !== null && !Array.isArray(base[k])
      ? deepMerge(base[k], v) : v;
  }
  return out;
}
