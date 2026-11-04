// ui-bench probe: a plain classic script that app-entry.mjs injects with CDP
// Page.addScriptToEvaluateOnNewDocument into the page's main world, before any app
// script runs. It observes and never acts:
//   - window.WebSocket and window.fetch are wrapped transparently (RosettaClient binds
//     both at construction); a message listener added in the constructor runs before
//     the app's onmessage and stamps receipt. /ws/meta frames and /api/status bodies go
//     to a Blob Worker (probe-worker.js) for parsing;
//   - a MutationObserver marks the DOM dirty and one requestAnimationFrame pass reads
//     the DOM contract (clients/app/test/dom-contract.test.mjs) and stamps the frame;
//   - PerformanceObservers record long animation frames, event timing and layout shift.
// Ids, titles and query text stay in page memory. The export (`finish`) holds only
// counts, sizes, timings (epoch ms on this page's clock: timeOrigin + now()) and enums,
// sent through the CDP binding __uiBenchEmit. Nothing is written to the DOM, every
// handler swallows its own errors, and the probe times its own main-thread work.
(function () {
  'use strict';
  // The main frame only: Page.addScriptToEvaluateOnNewDocument reaches every frame,
  // an artifact's srcdoc frame included, and the probe has no business there.
  try { if (window.top !== window) return; } catch { return; }
  if (window.__uiBench) return;
  const CFG = window.__uiBenchConfig || {};
  const WORKER_SRC = window.__uiBenchWorkerSrc;
  try { delete window.__uiBenchConfig; delete window.__uiBenchWorkerSrc; } catch { /* keep going */ }
  const P = performance;
  const ORIGIN = P.timeOrigin;
  const now = () => ORIGIN + P.now();
  const ev2epoch = (ts) => ORIGIN + ts;
  const cost = { mainMs: 0, postMs: 0, postBytes: 0, posts: 0, passes: 0, passMs: 0, apiMs: 0, observerCalls: 0, errors: 0 };
  const queued = [];
  function emit(e, extra) {
    const msg = JSON.stringify(Object.assign({ e, t: now() }, extra || {}));
    try {
      if (typeof window.__uiBenchEmit === 'function') {
        while (queued.length) window.__uiBenchEmit(queued.shift());
        window.__uiBenchEmit(msg);
      } else queued.push(msg);
    } catch { cost.errors += 1; }
  }
  function guard(fn, slot) {
    return function () {
      const a = P.now();
      try { return fn.apply(this, arguments); } catch { cost.errors += 1; return undefined; } finally {
        const d = P.now() - a;
        cost.mainMs += d;
        if (slot) cost[slot] += d;
      }
    };
  }
  const once = new Set();
  const emitOnce = (e, extra) => { if (!once.has(e)) { once.add(e); emit(e, extra); } };

  // ── export log (numbers and enums only) ──────────────────────────────────
  const L = {
    format: 'ui-probe/1',
    selfTest: { firstMetaAt: null, wsWrapped: false, fetchWrapped: false, worker: 'none', loaf: false, eventTiming: false, layoutShift: false },
    page: { firstFrameAt: null, firstRollCallAt: null, rollCallReadyAt: null, dpr: null, innerW: null, innerH: null, focusedAtStart: document.hasFocus(), focusedAtRollCall: null },
    faces: [], states: [], readyCount: [], readyRoots: [], notImported: [], marks: [], inputs: [],
    status: { rtt: [], aborted: 0, firstNonConsentAt: null, firstCanonicalAt: null, firstSearchReadyAt: null, firstLiveAt: null, metaReadyAtWall: null, firstPostConsentPhase: null, last: null, kinds: [], ingestKinds: [] },
    meta: { sockets: 0, firstProjectFrameAt: null, flipFrames: [], selFlips: [], retractions: 0 },
    frozen: null, h1: null,
    h2: { firstDrawAt: null, firstDrawStanding: null, indexAtFirstDraw: null, paintReadyAt: null, receiptAt: null, heldReadyMs: 0, newestExcluded: false },
    h3: { h24ReceiptAt: null, h7dReceiptAt: null, n24: null, n7d: null, n24Sel: null, n7dSel: null, h24PaintSelAt: null, h7dPaintSelAt: null,
      h24ViaCaptureOver: false, h7dViaCaptureOver: false, excluded24: null, excluded7d: null, excludedSel: null },
    hall: { paintSelAt: null },
    opens: [], h5: [], loaf: [], events: [], layoutShift: { sum: 0, sumNoInput: 0, n: 0 },
    // Two definitions of "unrequested" until the user picks one: no input of any kind
    // (pointer motion included) in the previous 500 ms, and no discrete input (press,
    // release, wheel, key) in the previous 500 ms. The first is the gated one for now.
    h7: { projects: { moves: 0, px: 0, movesDiscrete: 0, pxDiscrete: 0 }, sessions: { moves: 0, px: 0, movesDiscrete: 0, pxDiscrete: 0 },
      selectionJumps: 0, selectionJumpsDiscrete: 0, aims: [], heldMs: { projects: 0, sessions: 0 }, pillShown: { projects: 0, sessions: 0 } },
    h8: { chartsSelectedAt: null, chartsVisibleAt: null, chartsAllAt: null, portfolioAt: null, search: [] },
    // Each check also counts what it looked at, so a pass with nothing checked shows.
    i1: { c1Before: 0, c1PendingReady: 0, c2: 0, c3: 0, c3Discrete: 0, c3ByList: { projects: 0, sessions: 0 }, c3Frames: 0, selectionChanges: 0,
      c4: 0, c4Shown: 0, c4Indexing: 0, c4Opens: 0, c7: 0, c7Faces: 0, checkedRows: 0 },
    d1: { beginAt: null, progressAt: null, titleRowsAfterSearchMs: 0, sheetAfterSearch: false },
    d2: { rank: [], inversionSelMs: 0, selShown: false, giantReceiptAt: null, giantPaintAt: null, giantKnown: false },
    d3: { covers: 0, coversOver100: 0, labels: {}, recovers: 0, errorCovers: 0, sockets: 0, secondSockets: 0, sessionResyncs: 0, wsErrorFrames: 0, http503: 0, httpNotOk: 0, longestSnapshotWaitMs: null },
  };
  const FACE = new Set(['connecting', 'not-answering', 'consent', 'cold-start', 'roll-call-looking', 'roll-call-ready', 'roll-call-starting', 'cols', 'none']);
  const STATES = new Set(['disconnected', 'complete', 'discovering', 'metadata', 'restoring', 'reading', 'organizing', 'finalizing', 'publishing', 'paused', 'pausing', 'failed', 'search_failed', 'search_waiting', 'search_indexing', 'search_optimizing', 'search_validating', 'search_publishing', 'none']);
  const LABELS = new Set(['error', 'team', 'seeking', 'preparing', 'loading']);
  const RUNSTATE = new Set(['running', 'pausing', 'paused', 'succeeded', 'failed']);
  const COURSE = new Set(['newest-first', 'runtime-close', 'resumed', 'stopped']);
  // The face's course each daemon admission course allows (ImportCourse, room import-state.ts).
  const FACE_OF_COURSE = { per_tree: 'newest-first', at_runtime_close: 'runtime-close', at_publication: 'resumed' };
  let statusCourses = []; // in-page only, never exported
  const AGENTS = new Set(['antigravity', 'claude-code', 'codex', 'copilot', 'grok', 'hermes', 'kimi-code', 'opencode', 'pi']);
  const enumOf = (set, v) => (set.has(v) ? v : 'other');

  // ── page memory (never exported) ─────────────────────────────────────────
  const firstReady = new Map(); // session id -> receipt of its first non-Pending frame
  const rowsSeen = new Map(); // session id -> paint tracking
  const sockets = new Map(); // session id -> [socket records]
  let frozen = null; // {newest, newestProject, r24, r7, projectRoots(Map)}
  let phase = null;
  let canonicalSeen = false;
  let searchSeen = false;
  let lastInputAt = -Infinity;
  let lastDiscreteAt = -Infinity;
  let lastKeyAt = null;
  let giantId = null;
  let lastUnranked = null;
  let lastSearchAt = null;
  const opened = new Set();
  const aims = [];
  let openSample = null;
  const searchRuns = [];

  // ── Worker ───────────────────────────────────────────────────────────────
  let worker = null;
  const flushWaiters = new Map();
  let flushSeq = 0;
  try {
    if (typeof WORKER_SRC === 'string') {
      worker = new Worker(URL.createObjectURL(new Blob([WORKER_SRC], { type: 'text/javascript' })));
      L.selfTest.worker = 'worker';
      worker.onmessage = guard((e) => onWorker(e.data));
      worker.postMessage({ k: 'cfg', giantNative: CFG.giantNative ?? null });
    }
  } catch { worker = null; L.selfTest.worker = 'failed'; }
  function post(msg, bytes) {
    if (worker === null) return;
    const a = P.now();
    try { worker.postMessage(msg); } catch { cost.errors += 1; }
    cost.postMs += P.now() - a;
    cost.posts += 1;
    cost.postBytes += bytes || 0;
  }

  const PHASE = new Set(['discovering', 'awaiting_consent', 'backfilling', 'metadata_ready', 'restoring', 'finalizing', 'publishing', 'live']);
  const SEARCH = new Set(['ready', 'building', 'catching-up', 'stale', 'failed']);
  function onWorker(m) {
    if (m.k === 'meta') {
      for (const id of m.flips) if (!firstReady.has(id)) firstReady.set(id, m.t);
      if (m.flips.length) L.meta.flipFrames.push(m.t);
      L.meta.retractions += m.retract;
      if (m.hasProject && L.meta.firstProjectFrameAt === null && L.d1.beginAt !== null) L.meta.firstProjectFrameAt = m.t;
      if (frozen !== null) {
        for (const key of m.projFlips) {
          if (key === frozen.newestProject) {
            L.meta.selFlips.push(m.t);
            emit('flip-sel', { k: L.meta.selFlips.length });
          }
        }
      }
      if (m.readyRoots !== null) L.readyRoots.push([m.t, m.readyRoots, m.totalRoots]);
      if (m.unranked !== null) lastUnranked = m.unranked;
      if (m.giantId && giantId === null) { giantId = m.giantId; L.d2.giantKnown = true; }
      if (frozen !== null && m.totalRoots !== null && m.totalRoots > 0 && m.readyRoots >= m.totalRoots) allReadable('frames');
    } else if (m.k === 'status') {
      phase = m.phase;
      const S = L.status;
      const code = m.phase === null ? 'none' : enumOf(PHASE, m.phase);
      S.last = { phase: code, canonical: m.canonical, search: m.search === null ? 'none' : enumOf(SEARCH, m.search), done: m.done, total: m.total, run: m.run === null ? 'none' : enumOf(RUNSTATE, m.run) };
      statusCourses = (m.courses || []).map((c) => FACE_OF_COURSE[c]).filter(Boolean);
      S.kinds = m.kinds.map((k) => enumOf(AGENTS, k)); S.ingestKinds = m.ingestKinds.map((k) => enumOf(AGENTS, k));
      if (m.metaReadyAt !== null && S.metaReadyAtWall === null) S.metaReadyAtWall = m.metaReadyAt;
      if (L.d1.beginAt !== null && S.firstNonConsentAt === null && m.phase !== 'awaiting_consent' && m.phase !== null) {
        S.firstNonConsentAt = m.t; S.firstPostConsentPhase = code;
      }
      if (m.canonical && S.firstCanonicalAt === null && L.d1.beginAt !== null) { S.firstCanonicalAt = m.t; canonicalSeen = true; emitOnce('status-canonical'); }
      if (m.canonical && m.search === 'ready' && S.firstSearchReadyAt === null && L.d1.beginAt !== null) { S.firstSearchReadyAt = m.t; searchSeen = true; emitOnce('status-search'); }
      if (m.phase === 'live' && S.firstLiveAt === null && L.d1.beginAt !== null) { S.firstLiveAt = m.t; emitOnce('status-live'); }
      if (L.d1.beginAt !== null && (m.phase === 'finalizing' || m.phase === 'publishing' || m.phase === 'live')) allReadable('phase');
    } else if (m.k === 'frozen') {
      frozen = { newest: m.newest, newestProject: m.newestProject, r24: new Set(m.r24), r7: new Set(m.r7), projectRoots: new Map(Object.entries(m.projectRoots)) };
      const inSel = (set) => [...set].filter((id) => frozen.projectRoots.has(id));
      frozen.r24Sel = inSel(frozen.r24); frozen.r7Sel = inSel(frozen.r7);
      L.frozen = { at: m.t, roots: m.roots, metaOnly: m.metaOnly, newestKnown: m.newest !== null, projectRoots: frozen.projectRoots.size };
      L.h3.n24 = m.r24.length; L.h3.n7d = m.r7.length; L.h3.n24Sel = frozen.r24Sel.length; L.h3.n7dSel = frozen.r7Sel.length;
      emitOnce('frozen', { roots: m.roots });
      schedule(); // the H1 + 0 rank sample and the frozen-set legs need a pass now, not at the next mutation
    } else if (m.k === 'horizon') {
      L.h3[m.name + 'ReceiptAt'] = m.at;
      if (m.viaCaptureOver) L.h3[m.name + 'ViaCaptureOver'] = true;
      emitOnce(m.name);
    } else if (m.k === 'parsed') {
      const rec = m.ref !== undefined ? parseRefs.get(m.ref) : null;
      if (rec) { rec.parseMs = m.ms; parseRefs.delete(m.ref); }
    } else if (m.k === 'end' || m.k === 'flushed') {
      const w = flushWaiters.get(m.id); if (w) { flushWaiters.delete(m.id); w(m); }
    }
  }
  let allReadableSent = false;
  function allReadable(via) {
    if (allReadableSent) return;
    allReadableSent = true;
    emit('all-readable', { via: via === 'phase' ? 1 : 0 });
  }
  function workerCall(msg) {
    return new Promise((resolve) => {
      if (worker === null) { resolve(null); return; }
      const id = (flushSeq += 1);
      flushWaiters.set(id, resolve);
      post(Object.assign({ id }, msg), 0);
      setTimeout(() => { if (flushWaiters.delete(id)) resolve(null); }, 10_000);
    });
  }
  const parseRefs = new Map();
  let parseSeq = 0;

  // ── WebSocket ────────────────────────────────────────────────────────────
  const NativeWS = window.WebSocket;
  function wsKind(url) {
    try {
      const u = new URL(String(url), location.href);
      if (u.pathname === '/ws/meta') return ['meta', null];
      const m = u.pathname.match(/^\/ws\/sessions\/([^/]+)$/);
      if (m) return ['session', decodeURIComponent(m[1])];
    } catch { /* fall through */ }
    return ['other', null];
  }
  function attach(ws, url) {
    const [kind, sid] = wsKind(url);
    if (kind === 'meta') {
      L.meta.sockets += 1;
      ws.addEventListener('message', guard((event) => {
        const t = now();
        const d = typeof event.data === 'string' ? event.data : '';
        if (L.selfTest.firstMetaAt === null && d.slice(0, 48).includes('"meta_snapshot"')) {
          L.selfTest.firstMetaAt = t;
          emitOnce('first-meta');
        }
        post({ k: 'meta', t, d }, d.length * 2);
      }));
    } else if (kind === 'session') {
      const rec = { sid, createdAt: now(), openAt: null, helloAt: null, snapAt: null, snapBytes: null, parseMs: null };
      const list = sockets.get(sid) || [];
      list.push(rec); sockets.set(sid, list);
      L.d3.sockets += 1;
      // D3(d): a second socket for the session already open since this click (the
      // hold path's 3 s retry, or a reconnect), not a later re-open of it.
      if (openSample && openSample.key === sid && list.filter((r) => r.createdAt >= openSample.clickAt).length > 1) L.d3.secondSockets += 1;
      ws.addEventListener('open', guard(() => { if (rec.openAt === null) rec.openAt = now(); }));
      ws.addEventListener('message', guard((event) => {
        const t = now();
        const d = typeof event.data === 'string' ? event.data : '';
        const head = d.slice(0, 40);
        if (head.includes('"hello"')) { if (rec.helloAt === null) rec.helloAt = t; }
        else if (head.includes('"timeline_snapshot"')) {
          if (rec.snapAt === null) {
            rec.snapAt = t; rec.snapBytes = d.length;
            if (openSample && openSample.key === sid) { const ref = (parseSeq += 1); parseRefs.set(ref, rec); post({ k: 'parse', ref, d }, d.length * 2); }
          }
        } else if (head.includes('"resync"')) L.d3.sessionResyncs += 1;
        else if (head.includes('"error"')) L.d3.wsErrorFrames += 1;
      }));
    }
  }
  try {
    class WebSocket extends NativeWS {
      constructor(...args) {
        super(...args);
        try { attach(this, args[0]); } catch { cost.errors += 1; }
      }
    }
    window.WebSocket = WebSocket;
    L.selfTest.wsWrapped = true;
  } catch { cost.errors += 1; }

  // ── fetch ────────────────────────────────────────────────────────────────
  const nativeFetch = window.fetch;
  function route(input) {
    try {
      const u = new URL(typeof input === 'string' ? input : (input && input.url) || String(input), location.href);
      const p = u.pathname;
      if (p === '/api/status') return 'status';
      if (p === '/api/runtimes/enable') return 'enable';
      if (p === '/api/search') return 'search';
      return p.startsWith('/api/') ? 'api' : 'other';
    } catch { return 'other'; }
  }
  const observeFetch = guard((input, promise) => {
    const r = route(input);
    if (r === 'other') return;
    const sent = now();
    promise.then(guard((res) => {
      const at = now();
      if (res.status === 503) L.d3.http503 += 1;
      if (!res.ok) L.d3.httpNotOk += 1;
      if (r === 'search') lastSearchAt = at;
      if (r !== 'status') return;
      L.status.rtt.push([sent, at]);
      if (!res.ok) return;
      res.clone().text().then(guard((body) => post({ k: 'status', t: at, d: body }, body.length * 2)), () => {});
    }), guard(() => { if (r === 'status') L.status.aborted += 1; }));
  });
  try {
    window.fetch = function fetch(input, init) {
      const promise = nativeFetch.apply(window, arguments);
      observeFetch(input, promise);
      return promise;
    };
    L.selfTest.fetchWrapped = true;
  } catch { cost.errors += 1; }

  // ── input (capture, passive: observed, never handled) ────────────────────
  const INPUTS = ['pointerdown', 'pointerup', 'pointermove', 'wheel', 'keydown'];
  const onInput = guard((event) => {
    if (!event.isTrusted) return;
    const t = ev2epoch(event.timeStamp);
    lastInputAt = Math.max(lastInputAt, t);
    if (event.type === 'keydown') lastKeyAt = t;
    if (event.type === 'pointermove') return;
    lastDiscreteAt = Math.max(lastDiscreteAt, t);
    L.inputs.push([t, event.type]);
    if (event.type === 'pointerdown') onPointerDown(event, t);
  });
  for (const type of INPUTS) window.addEventListener(type, onInput, { capture: true, passive: true });

  function rowOf(target) {
    const el = target && target.closest ? target.closest('.ah-session-list-item[data-motion-key], .ah-project-list-item[data-motion-key]') : null;
    if (el === null) return null;
    return { el, list: el.classList.contains('ah-session-list-item') ? 'sessions' : 'projects', key: el.getAttribute('data-motion-key'), standing: el.getAttribute('data-standing') };
  }
  function onPointerDown(event, t) {
    const row = rowOf(event.target);
    const aim = aims.length ? aims[aims.length - 1] : null;
    if (aim && aim.hit === null && t - aim.at < 5000) {
      aim.hit = aim.match(event.target, row);
      // What the press met instead, and how far the target moved since its rect was read.
      const moved = aim.el.isConnected ? Math.round(aim.el.getBoundingClientRect().top - aim.top) : null;
      const met = aim.hit ? 'aimed' : row !== null ? 'other-row' : 'none';
      L.h7.aims.push([aim.at, aim.kind, aim.hit ? 1 : 0, moved, met]);
    }
    if (event.target && event.target.closest && event.target.closest('button[data-action="begin-ingest"]') && L.d1.beginAt === null) L.d1.beginAt = t;
    if (row && row.list === 'sessions') {
      opened.add(row.key);
      openSample = { key: row.key, clickAt: t, standing: row.standing === 'not-imported' ? 'not-imported' : row.standing === 'unavailable' ? 'unavailable' : 'ready',
        phase: phase === null ? 'none' : enumOf(PHASE, phase), canonical: canonicalSeen, contentAt: null, faceAt: null, course: null,
        coverAt: null, coverEndAt: null, label: null, revealAt: null, sock: null };
      L.opens.push(openSample);
      if (phase !== 'live') L.i1.c4Opens += 1; // an open while importing: where an empty face could flash
    }
  }

  // ── PerformanceObservers ─────────────────────────────────────────────────
  const EVENT_NAMES = new Set(['pointerdown', 'pointerup', 'click', 'keydown', 'keyup', 'keypress', 'input', 'mousedown', 'mouseup']);
  function observe(type, opts, fn, flag) {
    try {
      const o = new PerformanceObserver(guard((list) => { cost.observerCalls += 1; for (const e of list.getEntries()) fn(e); }));
      o.observe(Object.assign({ type, buffered: true }, opts));
      L.selfTest[flag] = true;
    } catch { L.selfTest[flag] = false; }
  }
  observe('long-animation-frame', {}, (e) => L.loaf.push([ev2epoch(e.startTime), e.duration, e.blockingDuration || 0]), 'loaf');
  observe('event', { durationThreshold: 16 }, (e) => {
    if (!EVENT_NAMES.has(e.name)) return;
    L.events.push([ev2epoch(e.startTime), e.duration, e.name, Number(e.interactionId) || 0, Math.max(0, e.processingStart - e.startTime)]);
  }, 'eventTiming');
  observe('layout-shift', {}, (e) => {
    L.layoutShift.sum += e.value; L.layoutShift.n += 1;
    if (!e.hadRecentInput) L.layoutShift.sumNoInput += e.value;
  }, 'layoutShift');

  // ── the frame pass ───────────────────────────────────────────────────────
  const Q = (sel, root) => (root || document).querySelector(sel);
  const QA = (sel, root) => (root || document).querySelectorAll(sel);
  function absTop(el) { let y = 0; for (let e = el; e; e = e.offsetParent) y += e.offsetTop; return y; }
  function faceNow() {
    if (Q('.ah-cols')) return 'cols';
    const rc = Q('[data-roll-call]');
    if (rc) return enumOf(FACE, 'roll-call-' + rc.getAttribute('data-roll-call'));
    const f = Q('.ah-empty[data-face]');
    return f ? enumOf(FACE, f.getAttribute('data-face')) : 'none';
  }
  function scanList(els, kind) {
    const out = [];
    let head = false;
    const sc = els.length ? els[els.length - 1].closest('.ah-scroll') : null;
    const scTop = sc ? absTop(sc) : 0;
    const scroll = sc ? sc.scrollTop : 0;
    const height = sc ? sc.clientHeight : 0;
    for (const el of els) {
      if (el.classList.contains('ah-day')) { head = el.hasAttribute('data-not-imported'); continue; }
      const top = absTop(el) - scTop - scroll;
      const h = el.offsetHeight;
      out.push({ el, key: el.getAttribute('data-motion-key'), top, visible: top + h > 0 && top < height,
        selected: el.getAttribute('aria-current') === 'true' || el.querySelector('[aria-current="true"]') !== null,
        standing: el.getAttribute('data-standing'), state: el.getAttribute('data-stream-state'),
        cooking: el.hasAttribute('data-cooking'), held: kind === 'sessions' && head, index: out.length });
    }
    return out;
  }
  const prev = { t: null, face: null, state: null, readyCount: null, pos: { projects: null, sessions: null }, sel: { projects: undefined, sessions: undefined },
    pill: { projects: false, sessions: false }, newestHeld: false, inversion: 0, titleRows: false, notImported: null, cover: null, rankAt: [] };
  function movement(list, rows, t) {
    const cur = new Map(rows.map((r) => [r.key, r]));
    const before = prev.pos[list];
    const quiet = t - lastInputAt > 500;
    const quietDiscrete = t - lastDiscreteAt > 500;
    const H = L.h7[list];
    if (before && quietDiscrete) {
      for (const [key, r] of cur) {
        const b = before.get(key);
        if (!b || !(b.visible || r.visible)) continue;
        const d = Math.abs(r.top - b.top);
        if (d >= 8) {
          H.movesDiscrete += 1; H.pxDiscrete += d;
          if (quiet) { H.moves += 1; H.px += d; }
        }
      }
    }
    if (rows.length) L.i1.c3Frames += 1;
    const sel = rows.find((r) => r.selected) || null;
    const selKey = sel ? sel.key : null;
    if (prev.sel[list] !== undefined && prev.sel[list] !== selKey) {
      L.i1.selectionChanges += 1;
      if (quiet) { L.i1.c3 += 1; L.i1.c3ByList[list] += 1; }
      if (quietDiscrete) L.i1.c3Discrete += 1;
    }
    if (before && sel && quietDiscrete) {
      const b = before.get(sel.key);
      if (b && (Math.abs(sel.top - b.top) >= 8 || (b.visible && !sel.visible))) {
        L.h7.selectionJumpsDiscrete += 1;
        if (quiet) L.h7.selectionJumps += 1;
      }
    }
    prev.sel[list] = selKey;
    prev.pos[list] = cur;
  }
  function trackRows(rows, t) {
    const present = new Set();
    for (const r of rows) {
      present.add(r.key);
      let s = rowsSeen.get(r.key);
      if (!s) { s = { since: null, lastNI: null, readyAt: null, readyState: null, readySince: null, everReady: false }; rowsSeen.set(r.key, s); }
      if (s.since === null) s.since = t;
      if (r.standing === 'not-imported') {
        if (s.everReady && s.lastStanding !== 'not-imported') L.i1.c2 += 1; // one per revert, not per frame
        s.lastNI = t;
      } else if (r.standing === 'ready') {
        if (s.readyAt === null) {
          s.readyAt = t; s.readyState = r.state; s.readySince = s.since;
          L.i1.checkedRows += 1;
        }
        // A Pending row reads ready only once the import is over; judged at finish
        // against the receipt of the first status that said so.
        if (r.state === 'pending' && s.readyPendingAt == null) s.readyPendingAt = t;
        if (r.state !== 'pending') s.everReady = true;
      }
      s.lastStanding = r.standing;
    }
    for (const [key, s] of rowsSeen) if (!present.has(key)) s.since = null;
  }
  function pass() {
    const t = now();
    const dt = prev.t === null ? 0 : t - prev.t;
    cost.passes += 1;
    if (L.page.firstFrameAt === null) L.page.firstFrameAt = t;
    const face = faceNow();
    if (face !== prev.face) {
      L.faces.push([t, face]);
      if (face.startsWith('roll-call') && L.page.firstRollCallAt === null) L.page.firstRollCallAt = t;
      if (face === 'roll-call-ready' && L.page.rollCallReadyAt === null) { L.page.rollCallReadyAt = t; L.page.focusedAtRollCall = document.hasFocus(); emitOnce('rollcall-ready'); }
      prev.face = face;
    }
    const card = Q('.ah-ingest-status');
    const state = card ? enumOf(STATES, card.getAttribute('data-state')) : 'none';
    if (state !== prev.state) { L.states.push([t, state]); prev.state = state; }
    const rc = card ? Number(card.getAttribute('data-ready-count')) : null;
    if (rc !== null && rc !== prev.readyCount) { L.readyCount.push([t, rc]); prev.readyCount = rc; }
    if (L.d1.beginAt !== null && L.d1.progressAt === null && t > L.d1.beginAt && (card || face === 'cols')) L.d1.progressAt = t;

    const projects = face === 'cols' ? scanList(QA('.ah-project-list-item[data-motion-key]'), 'projects') : [];
    const sessions = face === 'cols' ? scanList(QA('.ah-session-list .ah-day, .ah-session-list .ah-session-list-item[data-motion-key]'), 'sessions') : [];
    if (L.h1 === null && face === 'cols' && projects.length > 0 && L.d1.beginAt !== null) {
      L.h1 = { paintAt: t, projects: projects.length };
      emitOnce('cols-up', { projects: projects.length });
      for (const off of [0, 5000, 30000]) prev.rankAt.push([off, t + off]);
      setTimeout(schedule, 5001); setTimeout(schedule, 30001);
    }
    movement('projects', projects, t);
    movement('sessions', sessions, t);
    trackRows(sessions, t);

    // Holds (H7 d): the session list's pill, the project list's pill.
    for (const [list, sel] of [['sessions', '.ah-list-view:has(> .ah-session-list) > .ah-list-updates'], ['projects', '.ah-list-view:has(.ah-project-list-item) > .ah-list-updates']]) {
      if (prev.pill[list]) L.h7.heldMs[list] += dt;
      const shown = Q(sel + '[data-visible]') !== null;
      if (shown && !prev.pill[list]) L.h7.pillShown[list] += 1;
      prev.pill[list] = shown;
    }

    const byKey = new Map(sessions.map((r) => [r.key, r]));
    if (frozen !== null) {
      // H2 — NEWEST's row.
      if (prev.newestHeld) L.h2.heldReadyMs += dt;
      const nr = byKey.get(frozen.newest) || null;
      prev.newestHeld = nr !== null && nr.standing === 'ready' && nr.held;
      if (nr) {
        if (L.h2.firstDrawAt === null) { L.h2.firstDrawAt = t; L.h2.firstDrawStanding = nr.standing === 'not-imported' ? 'not-imported' : 'ready'; L.h2.indexAtFirstDraw = nr.index; emitOnce('newest-drawn'); }
        if (nr.standing === 'ready' && L.h2.paintReadyAt === null) { L.h2.paintReadyAt = t; emitOnce('newest-ready'); }
      }
      // H3 painted legs, H-all painted leg, D2(b) in the list.
      const allReady = (ids) => ids.every((id) => { const r = byKey.get(id); return r && r.standing === 'ready'; });
      if (L.h3.h24PaintSelAt === null && frozen.r24Sel.length > 0 && allReady(frozen.r24Sel)) L.h3.h24PaintSelAt = t;
      if (L.h3.h7dPaintSelAt === null && frozen.r7Sel.length > 0 && allReady(frozen.r7Sel)) L.h3.h7dPaintSelAt = t;
      const showsNewestProject = sessions.some((r) => frozen.projectRoots.has(r.key));
      if (L.hall.paintSelAt === null && showsNewestProject && !sessions.some((r) => r.standing === 'not-imported')) L.hall.paintSelAt = t;
      L.d2.inversionSelMs += prev.inversion * dt;
      if (showsNewestProject) L.d2.selShown = true;
      let inv = 0;
      if (showsNewestProject) {
        let oldestReady = Infinity;
        for (const r of sessions) {
          const u = frozen.projectRoots.get(r.key);
          if (u === undefined) continue;
          if (frozen.r24.has(r.key) && oldestReady < u) inv += 1;
          if (r.standing === 'ready' && u < oldestReady) oldestReady = u;
        }
      }
      prev.inversion = inv;
      if (prev.rankAt.length && t >= prev.rankAt[0][1]) {
        const [off] = prev.rankAt.shift();
        const at = projects.findIndex((p) => p.key === frozen.newestProject);
        L.d2.rank.push([off, t, at, lastUnranked === null ? -1 : lastUnranked]);
      }
    }
    if (giantId !== null && L.d2.giantPaintAt === null) { const g = byKey.get(giantId); if (g && g.standing === 'ready') L.d2.giantPaintAt = t; }
    const ni = sessions.filter((r) => r.standing === 'not-imported').length;
    if (sessions.length && ni !== prev.notImported) { L.notImported.push([t, ni]); prev.notImported = ni; }

    // Transcript (H4, D3, I1 c4).
    const stage = Q('.tr-stage[data-session-key]');
    const skey = stage ? stage.getAttribute('data-session-key') : null;
    if (stage) {
      const ready = stage.getAttribute('data-ready') === 'true';
      const sitting = stage.querySelector('.tr-sitting[data-sitting-key]') !== null;
      const importFace = stage.querySelector('[data-import-face]');
      const loading = stage.querySelector('.tr-loading');
      const s = rowsSeen.get(skey) || null;
      // I1(4): "Nothing readable yet" is the generic empty face of an INDEXED session;
      // an unindexed one shows "Indexing history" instead (counted, not a violation).
      if (ready && !sitting && !importFace && stage.querySelector('.tr-surface .ah-empty') && phase !== 'live') {
        const indexed = (byKey.get(skey) || {}).state === 'indexed';
        const rec = s || { since: null, lastNI: null, readyAt: null, readyState: null, readySince: null, everReady: false };
        if (!s) rowsSeen.set(skey, rec);
        if (indexed && !rec.emptyShown) { rec.emptyShown = true; L.i1.c4Shown += 1; }
        if (!indexed && !rec.indexingShown) { rec.indexingShown = true; L.i1.c4Indexing += 1; }
      }
      if (sitting && s && s.emptyShown && !s.c4) { s.c4 = true; L.i1.c4 += 1; }
      if (loading) {
        const label = enumOf(LABELS, loading.getAttribute('data-loading-label'));
        if (prev.cover === null) { prev.cover = { at: t, key: skey, label }; L.d3.covers += 1; L.d3.labels[label] = (L.d3.labels[label] || 0) + 1; if (label === 'error') L.d3.errorCovers += 1; }
      } else if (prev.cover !== null) { if (t - prev.cover.at > 100) L.d3.coversOver100 += 1; prev.cover = null; }
      const o = openSample;
      if (o && o.key === skey) {
        if (loading && o.coverAt === null) { o.coverAt = t; o.label = enumOf(LABELS, loading.getAttribute('data-loading-label')); }
        if (!loading && o.coverAt !== null && o.coverEndAt === null) o.coverEndAt = t;
        if (ready && o.revealAt === null) o.revealAt = t;
        if (!ready && o.contentAt !== null && !o.recovered) { o.recovered = true; L.d3.recovers += 1; }
        if (ready && sitting && !importFace && o.contentAt === null) { o.contentAt = t; emit('content', { n: L.opens.length }); }
        if (importFace && importFace.getAttribute('data-import-face') === 'not-imported' && o.faceAt === null) {
          o.faceAt = t; o.course = enumOf(COURSE, importFace.getAttribute('data-import-course'));
          // Stopped runs promise only a continue; otherwise the face must name a course
          // some runtime's status reports (a daemon that names none: newest-first).
          const stopped = L.status.last && (L.status.last.run === 'paused' || L.status.last.run === 'pausing' || L.status.last.run === 'failed');
          const allowed = stopped ? ['stopped'] : (statusCourses.length ? statusCourses : ['newest-first']);
          L.i1.c7Faces += 1;
          if (!allowed.includes(o.course)) L.i1.c7 += 1;
          emit('pending-face', { n: L.opens.length });
        }
      }
    } else if (prev.cover !== null) { prev.cover = null; }

    // Charts and the portfolio (H8 a).
    if (canonicalSeen && face === 'cols') {
      const selCard = projects.find((p) => p.selected) || null;
      const veiledOutside = [...QA('.ah-cols .ah-veil')].some((v) => { const col = v.closest('.ah-col'); return col && !col.querySelector('.ah-project-list-item'); });
      if (L.h8.chartsSelectedAt === null && selCard && !selCard.cooking && !veiledOutside && (frozen === null || selCard.key === frozen.newestProject)) { L.h8.chartsSelectedAt = t; emitOnce('charts-selected'); }
      const vis = projects.filter((p) => p.visible);
      if (L.h8.chartsVisibleAt === null && vis.length && vis.every((p) => !p.cooking)) L.h8.chartsVisibleAt = t;
      if (L.h8.chartsAllAt === null && projects.length && projects.every((p) => !p.cooking)) { L.h8.chartsAllAt = t; emitOnce('charts-all'); }
      if (L.h8.portfolioAt === null && !card) { L.h8.portfolioAt = t; emitOnce('portfolio'); }
    }
    // The search sheet (H8 b, D1 f).
    const sheetRows = QA('.ah-sheet__row[data-hit-kind]');
    let full = 0; let title = 0;
    for (const r of sheetRows) { if (r.getAttribute('data-hit-kind') === 'fulltext') full += 1; else title += 1; }
    if (searchSeen && Q('.ah-sheet__field') !== null) L.d1.sheetAfterSearch = true;
    if (prev.titleRows && searchSeen) L.d1.titleRowsAfterSearchMs += dt;
    prev.titleRows = title > 0;
    const run = searchRuns.length ? searchRuns[searchRuns.length - 1] : null;
    if (run && run.digest === null && run.typedAt !== null) {
      if (full > 0 && run.firstFulltextAt === null) { run.firstFulltextAt = t; emit('fulltext', { q: run.q }); }
      if (title > 0 && run.firstTitleAt === null) run.firstTitleAt = t;
      const sig = [...sheetRows].map((r) => r.getAttribute('data-hit-kind') + ':' + r.id).join('|');
      if (sig !== run.sig) { run.sig = sig; run.sigAt = t; }
    }
    prev.t = t;
    cost.passMs += now() - t;
  }
  // A query has settled once the daemon answered after its last key and the rows have
  // not changed for 500 ms (a settle rule, not an aim). Then the UI hit digest:
  // sha256 over (query index, row index, kind, the row's DOM id) of what is drawn.
  const quietCheck = guard((run) => {
    if (run.digest !== null) return;
    const t = now();
    const answered = run.firstFulltextAt !== null || (lastSearchAt !== null && lastSearchAt > run.typedAt);
    if (!answered || t - (run.sigAt ?? run.typedAt) < 500 || (lastSearchAt !== null && t - lastSearchAt < 500)) {
      run.timer = setTimeout(quietCheck, 250, run);
      return;
    }
    const rows = [...QA('.ah-sheet__row[data-hit-kind]')];
    run.rows = rows.length;
    run.fulltextRows = rows.filter((r) => r.getAttribute('data-hit-kind') === 'fulltext').length;
    const text = rows.map((r, i) => `${run.q}\t${i}\t${r.getAttribute('data-hit-kind')}\t${r.id}\n`).join('');
    run.digest = 'pending';
    run.quietAt = now();
    crypto.subtle.digest('SHA-256', new TextEncoder().encode(text)).then((buf) => {
      run.digest = [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, '0')).join('');
      emit('sheet-quiet', { q: run.q, rows: run.rows });
    }, () => { run.digest = 'failed'; emit('sheet-quiet', { q: run.q, rows: run.rows }); });
  });
  let scheduled = false;
  const frame = guard(() => { scheduled = false; pass(); }, null);
  function schedule() { if (!scheduled) { scheduled = true; requestAnimationFrame(frame); } }
  const ATTRS = ['data-standing', 'data-stream-state', 'aria-current', 'data-ready', 'data-session-key', 'data-state', 'data-ready-count',
    'data-face', 'data-roll-call', 'data-cooking', 'data-visible', 'data-import-face', 'data-loading-label', 'data-hit-kind', 'class', 'style', 'hidden'];
  const mo = new MutationObserver(guard(() => { cost.observerCalls += 1; schedule(); }));
  function startObserving() {
    try { mo.observe(document, { subtree: true, childList: true, attributes: true, attributeFilter: ATTRS }); } catch { cost.errors += 1; }
    schedule();
  }
  if (document.documentElement) startObserving(); else document.addEventListener('DOMContentLoaded', startObserving, { once: true });
  window.addEventListener('scroll', guard(schedule), { capture: true, passive: true });

  // ── targets for the reader (rects only; ids stay here) ───────────────────
  let aimSeq = 0;
  function target(spec) {
    const [kind, arg] = String(spec).split(':');
    const rowsIn = (sel) => [...QA(sel)];
    const sessRows = () => rowsIn('.ah-session-list .ah-session-list-item[data-motion-key]');
    const key = (el) => el.getAttribute('data-motion-key');
    let el = null;
    if (kind === 'begin') el = Q('button[data-action="begin-ingest"]');
    else if (kind === 'kind') el = Q(`input[type=checkbox][data-agent-kind="${CSS.escape(arg || '')}"]`);
    else if (kind === 'newest-project' && frozen) el = rowsIn('.ah-project-list-item[data-motion-key]').find((e) => key(e) === frozen.newestProject) || null;
    else if (kind === 'other-project' && frozen) el = rowsIn('.ah-project-list-item[data-motion-key]').filter((e) => key(e) !== frozen.newestProject)[Number(arg || 1) - 1] || null;
    else if (kind === 'newest-row' && frozen) el = sessRows().find((e) => key(e) === frozen.newest) || null;
    else if (kind === 'recent-next' && frozen) el = sessRows().find((e) => frozen.r24.has(key(e)) && e.getAttribute('data-standing') === 'ready' && !opened.has(key(e))) || null;
    else if (kind === 'ready-next') el = sessRows().find((e) => e.getAttribute('data-standing') === 'ready' && !opened.has(key(e))) || sessRows().find((e) => e.getAttribute('data-standing') === 'ready') || null;
    else if (kind === 'pending-row') el = sessRows().find((e) => e.getAttribute('data-standing') === 'not-imported') || null;
    else if (kind === 'park') el = Q('.tr-stage') || Q('.ah-cols') || document.body;
    return el;
  }
  function rectOf(spec) {
    const el = target(spec);
    if (!el) return null;
    const r = el.getBoundingClientRect();
    const sc = el.closest('.ah-scroll');
    const s = sc ? sc.getBoundingClientRect() : null;
    const within = s === null || (r.top >= s.top && r.bottom <= s.bottom);
    const visible = within && r.bottom > 0 && r.top < window.innerHeight && r.width > 0;
    const out = { x: r.left, y: r.top, w: r.width, h: r.height, visible, dir: s === null || within ? 0 : (r.top < s.top ? -1 : 1),
      sx: s ? s.left + s.width / 2 : null, sy: s ? s.top + s.height / 2 : null, aimId: null };
    if (visible && spec !== 'park') {
      const id = (aimSeq += 1);
      const rowKey = el.getAttribute('data-motion-key');
      const kind = String(spec).split(':')[0];
      aims.push({ id, kind, at: now(), hit: null, el, top: r.top, match: (t, row) => (rowKey !== null ? row !== null && row.key === rowKey : (t === el || el.contains(t))) });
      out.aimId = id;
    }
    return out;
  }
  const API = {
    ping: () => now(),
    rect: (spec) => rectOf(spec),
    aimHit: (id) => { const a = aims.find((x) => x.id === id); return a ? a.hit : null; },
    rollcall: () => [...QA('input[type=checkbox][data-agent-kind]')].map((e) => ({ kind: e.getAttribute('data-agent-kind'), checked: e.checked, disabled: e.disabled })),
    state: () => {
      const rows = [...QA('.ah-session-list .ah-session-list-item[data-motion-key]')];
      const sel = Q('.ah-project-list-item[aria-current="true"], .ah-project-list-item.is-selected');
      return { face: faceNow(), frozen: frozen !== null, newestSelected: frozen !== null && sel !== null && sel.getAttribute('data-motion-key') === frozen.newestProject,
        pendingRows: rows.filter((e) => e.getAttribute('data-standing') === 'not-imported').length, readyRows: rows.filter((e) => e.getAttribute('data-standing') === 'ready').length,
        sheetOpen: Q('.ah-sheet__field') !== null, canonical: canonicalSeen, search: searchSeen, phase: phase === null ? 'none' : enumOf(PHASE, phase) };
    },
    mark: (name) => { L.marks.push([now(), String(name)]); return true; },
    searchBegin: (q) => { const run = { q: Number(q), beginAt: now(), typedAt: null, firstFulltextAt: null, firstTitleAt: null, quietAt: null, rows: null, fulltextRows: null, sig: null, sigAt: null, timer: null, digest: null }; searchRuns.push(run); schedule(); return true; },
    searchTyped: (q) => {
      const run = searchRuns.find((r) => r.q === Number(q));
      if (!run) return false;
      run.typedAt = lastKeyAt !== null && lastKeyAt >= run.beginAt ? lastKeyAt : now();
      run.sigAt = null; run.sig = null;
      schedule();
      run.timer = setTimeout(quietCheck, 250, run);
      return true;
    },
    searchDigest: (q) => { const run = searchRuns.find((r) => r.q === Number(q)); return run && run.digest !== 'pending' ? run.digest : null; },
    finish: (arg) => finish(arg),
  };
  async function finish(arg) {
    const daemon = arg && arg.daemon ? arg.daemon : null;
    pass();
    Object.assign(L.page, { dpr: window.devicePixelRatio, innerW: window.innerWidth, innerH: window.innerHeight });
    const end = await workerCall({ k: 'end', t: now(), daemon });
    // Rank samples whose moment never came before the end: recorded as not reached.
    for (const [off] of prev.rankAt) L.d2.rank.push([off, null, null, null]);
    prev.rankAt = [];
    // The count unit at the end (probe-worker endState): members of the frozen sets
    // still Pending once the import is over are never-rostered; the receipt legs come
    // from the worker over the rest, and a painted leg that lost a member is
    // recomputed over the rest from each row's first ready paint (rows never revert:
    // I1 c2). If NEWEST itself was never rostered, H2 follows the newest root that was.
    const eff = end && end.k === 'end' && frozen !== null ? end.eff : null;
    if (eff) {
      L.h3.h24ReceiptAt = eff.h24At; L.h3.h7dReceiptAt = eff.h7dAt;
      L.h3.excluded24 = eff.excluded24; L.h3.excluded7d = eff.excluded7d; L.h3.excludedSel = eff.excludedProject;
      L.h3.n24 = eff.n24; L.h3.n7d = eff.n7d;
      const inSel = (ids) => ids.filter((id) => frozen.projectRoots.has(id));
      const paintedAll = (ids) => {
        let last = null;
        for (const id of ids) { const s = rowsSeen.get(id); if (!s || s.readyAt === null) return null; if (last === null || s.readyAt > last) last = s.readyAt; }
        return last;
      };
      const sel24 = inSel(eff.r24);
      const sel7 = inSel(eff.r7);
      L.h3.n24Sel = sel24.length; L.h3.n7dSel = sel7.length;
      if (eff.excluded24 > 0) L.h3.h24PaintSelAt = sel24.length ? paintedAll(sel24) : null;
      if (eff.excluded7d > 0) L.h3.h7dPaintSelAt = sel7.length ? paintedAll(sel7) : null;
      if (eff.excludedProject > 0) L.hall.paintSelAt = eff.projectRoots.length ? paintedAll(eff.projectRoots) : null;
      if (eff.newestExcluded) {
        const s = eff.newest === null ? null : rowsSeen.get(eff.newest);
        Object.assign(L.h2, { newestExcluded: true, firstDrawAt: null, firstDrawStanding: null, indexAtFirstDraw: null, heldReadyMs: null, paintReadyAt: s ? s.readyAt : null });
      }
    }
    // I1(1): no root painted ready (non-Pending) before the frame that made it so arrived.
    for (const [key, s] of rowsSeen) {
      if (s.readyPendingAt != null && (L.status.firstLiveAt === null || s.readyPendingAt < L.status.firstLiveAt)) L.i1.c1PendingReady += 1;
      if (s.readyAt === null || s.readyState === 'pending') continue;
      const at = firstReady.get(key);
      if (at === undefined || at > s.readyAt) L.i1.c1Before += 1;
      // H5: drawn not-imported at receipt, continuously until its ready paint.
      if (at !== undefined && s.lastNI !== null && s.readySince !== null && s.readySince <= at && at <= s.readyAt) L.h5.push([at, s.readyAt]);
    }
    if (frozen !== null) L.h2.receiptAt = eff && eff.newestExcluded ? eff.newestReceiptAt : (firstReady.get(frozen.newest) ?? null);
    if (giantId !== null) L.d2.giantReceiptAt = firstReady.get(giantId) ?? null;
    let longest = null;
    for (const list of sockets.values()) for (const r of list) if (r.snapAt !== null && (longest === null || r.snapAt - r.createdAt > longest)) longest = r.snapAt - r.createdAt;
    L.d3.longestSnapshotWaitMs = longest;
    const opens = L.opens.map((o) => {
      const sock = (sockets.get(o.key) || []).find((r) => r.createdAt >= o.clickAt - 50) || null;
      return { clickAt: o.clickAt, standing: o.standing, phase: o.phase, canonical: o.canonical, contentAt: o.contentAt, faceAt: o.faceAt,
        course: o.course === null ? 'none' : enumOf(COURSE, o.course), coverAt: o.coverAt, coverEndAt: o.coverEndAt, label: o.label === null ? 'none' : o.label, revealAt: o.revealAt,
        wsAt: sock ? sock.createdAt : null, openAt: sock ? sock.openAt : null, helloAt: sock ? sock.helloAt : null, snapAt: sock ? sock.snapAt : null,
        snapBytes: sock ? sock.snapBytes : null, parseMs: sock ? sock.parseMs : null };
    });
    const search = searchRuns.map((r) => ({ q: r.q, beginAt: r.beginAt, typedAt: r.typedAt, firstFulltextAt: r.firstFulltextAt, firstTitleAt: r.firstTitleAt,
      quietAt: r.quietAt, rows: r.rows, fulltextRows: r.fulltextRows, digest: /^[0-9a-f]{64}$/.test(r.digest || '') ? r.digest : null }));
    const listed = [...QA('.ah-session-list .ah-session-list-item[data-motion-key]')].map((e) => e.getAttribute('data-motion-key'));
    const rendered = frozen !== null && listed.length > 0 && listed.every((k) => frozen.projectRoots.has(k)) ? listed.length : null;
    const out = Object.assign({}, L, { renderedNewestProjectRows: rendered, opens, h8: Object.assign({}, L.h8, { search }), cost: Object.assign({}, cost),
      end: end && end.k === 'end' ? { rootsSeen: end.rootsSeen, readyAll: end.readyAll, sessionsSeen: end.sessionsSeen, pendingSessions: end.pendingSessions, pendingRoots: end.pendingRoots,
        metaOnlyRoots: end.metaOnlyRoots, neverRostered: end.neverRostered, lastRootReady: end.lastRootReady, daemonRoots: end.daemonRoots,
        daemonPending: end.daemonPending, daemonTruncated: end.daemonTruncated, projectMismatches: end.projectMismatches,
        newestProjectRoots: end.newestProjectRoots, newestProjectDaemonRoots: end.newestProjectDaemonRoots, membershipStable: end.membershipStable,
        inversionMs: end.inversionMs, workerParseMs: end.parseMs, frames: end.counts.frames, snapshots: end.counts.snapshots, mutations: end.counts.mutations,
        resyncs: end.counts.resyncs, metaBytes: end.counts.bytes, statusBodies: end.counts.statusBodies, retractions: end.counts.retractions } : null,
      finishedAt: now() });
    emit('result', { data: out, nonce: arg && typeof arg.nonce === 'string' ? arg.nonce : null });
    return true;
  }
  // The reader's queries run on this thread too, so they are timed as probe work.
  window.__uiBench = Object.freeze({ q: guard((op, arg) => { const f = API[op]; return f ? f(arg) : null; }, 'apiMs') });
})();
