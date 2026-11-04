// rosetta's app over a simulated cold import. The page mounts the app's own
// App (renderer/src/App.tsx: HistoryRoom with the search sheet and its keys,
// the roll call as the consent screen, no StrictMode, as main.tsx does) on a
// host built the way renderer/src/host/rosetta-host.ts builds one: a single
// RosettaClient, the room's catalog over it through catalogOver, the roll
// call on the same client. The client's REST asks go to the simulated
// daemon (room/src/fixtures/cold-import.ts, its `daemon.fetch`), so the roll
// call, the SDK, catalogOver and the room all run their real code. Only what
// a fixture has no socket for stays the simulator's: /ws/meta and the
// session sockets. A change anywhere on that path that re-asks status after
// consent (ux:X7) therefore reaches the simulator's trail.
//
// The driver, cold-import.electron.mjs, steps simulated time and reads the
// room through the inert DOM contract. This page adds a probe that watches
// the zero-tolerance honesty rules (the metrics catalog's I1) on every DOM
// change, and a frame sampler for the end's row motion.
import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { RosettaClient } from '@vibefield/rosetta-protocol';
import App from '../renderer/src/App';
import { catalogOver } from '../renderer/src/host/rosetta-ports';
import { createColdImport } from '../../room/src/fixtures/cold-import';
import '../renderer/src/styles/tokens.css';
import '../renderer/src/styles/app.css';
import '../../room/src/styles/history-room.css';

// The import clock's zero is local noon on a fixed day, so "Today" spans the
// same twelve hours either side in every timezone. The room's clock follows
// simulated time: Date.now is the daemon's start plus the controller's clock.
const NOW = new Date(2026, 9, 2, 12).getTime();
const realNow = Date.now.bind(Date);
let current = null;
Date.now = () => (current === null ? realNow() : current.sim.controller.now + current.sim.controller.clock);

/** rosetta's host (renderer/src/host/rosetta-host.ts) over the simulator. */
function hostOver(sim) {
  const client = new RosettaClient({ baseUrl: 'http://127.0.0.1:4640', fetchFn: sim.daemon.fetch });
  // No sockets in a fixture: the metadata feed is the simulator's own.
  client.subscribeMeta = (handlers) => sim.daemon.subscribeMeta(handlers);
  const daemonListeners = new Set();
  const onDaemonState = (listener) => {
    daemonListeners.add(listener);
    return () => { daemonListeners.delete(listener); };
  };
  const catalog = catalogOver(client, { unavailable: () => null, onChange: onDaemonState });
  return {
    client,
    ports: {
      // The probe's one tap: each connection change that reaches the room
      // goes on the simulator's trail, wherever it came from.
      catalog: { ...catalog, onConnectionChange: (listener) => catalog.onConnectionChange(() => {
        sim.controller.mark('connection-change');
        listener();
      }) },
      sessions: sim.ports.sessions,
      assets: sim.ports.assets,
      ingestion: client.ingestions,
      host: null,
    },
    daemonState: () => null,
    onDaemonState,
    /** What the shell's daemon-state push does when the route changes. */
    routeChange() {
      const listeners = [...daemonListeners];
      for (const listener of listeners) listener();
      return listeners.length;
    },
  };
}

/* ── the probe (metrics I1, as far as a fixture can see) ──────────────── */

function createProbe() {
  const fresh = () => ({
    selections: 0, lastSelection: Number.NEGATIVE_INFINITY, selection: null,
    standings: new Map(), everReady: new Set(), opened: {},
    violations: [], faces: new Set(), cards: [], flipPaint: [],
    longTasks: { count: 0, maxMs: 0, totalMs: 0 },
  });
  const state = fresh();
  const violate = (check, detail) => {
    if (state.violations.length < 50) state.violations.push({ check, detail, clock: current?.sim.controller.clock ?? null });
  };
  const selected = () => {
    const at = (selector) => document.querySelector(selector)?.closest('[data-motion-key]')?.dataset.motionKey ?? null;
    return `${at('.ah-project-list-item[aria-current="true"], .ah-project-list-item__hit[aria-current="true"]')}|${
      at('.ah-session-list-item[aria-current="true"], .ah-session-list-item__hit[aria-current="true"]')}`;
  };
  const sweep = () => {
    const sim = current?.sim;
    if (sim === undefined) return;
    const c = sim.controller;
    const live = c.phase() === 'live';
    // c1/c2: a row reads ready only once the catalog made it non-Pending,
    // and a row once ready never reads not-imported again.
    for (const row of document.querySelectorAll('.ah-session-list-item[data-standing]')) {
      const id = row.dataset.motionKey;
      const standing = row.dataset.standing;
      if (state.standings.get(id) === standing) continue;
      state.standings.set(id, standing);
      if (standing === 'ready') {
        if (!live && c.streamState(id) === 'pending') violate('c1 ready before the catalog said so', id.slice(0, 8));
        state.everReady.add(id);
      } else if (standing === 'not-imported' && state.everReady.has(id)) {
        violate('c2 a ready row reverted to not imported', id.slice(0, 8));
      }
    }
    // c3: arriving data never selects. A selection change needs a scripted
    // selecting input (a click) within the previous 500 ms (I1(3)).
    const selection = selected();
    if (state.selection !== null && selection !== state.selection && performance.now() - state.lastSelection > 500) {
      violate('c3 selection changed without a click in the previous 500 ms', `${state.selection} → ${selection}`);
    }
    state.selection = selection;
    // c4: no flash of empty — never "Nothing readable yet" in the transcript.
    for (const face of document.querySelectorAll('.ah-col--measure .ah-empty strong')) {
      const text = face.textContent;
      state.faces.add(text);
      if (text === 'Nothing readable yet') violate('c4 "Nothing readable yet" shown', c.clock);
    }
    // When each opened session first drew rows, in simulated ms.
    const stage = document.querySelector('.ah-col--measure .tr-stage[data-session-key]');
    const key = stage?.dataset.sessionKey;
    if (key !== undefined && !(key in state.opened) && stage.querySelector('.tr-sitting')) state.opened[key] = c.clock;
    // The card's step, as the room drew it, against the simulated time.
    const card = document.querySelector('.ah-ingest-status')?.dataset.state ?? null;
    if (state.cards.at(-1)?.state !== card) {
      const canonical = c.phaseAt('canonical');
      state.cards.push({ state: card, clock: c.clock, phase: c.phase(), canonicalReady: canonical !== null && c.clock >= canonical });
    }
  };
  new MutationObserver(sweep).observe(document.body, {
    subtree: true, childList: true, attributes: true,
    attributeFilter: ['data-standing', 'aria-current', 'data-state', 'data-import-face'],
  });
  try {
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        state.longTasks.count += 1;
        state.longTasks.totalMs += entry.duration;
        state.longTasks.maxMs = Math.max(state.longTasks.maxMs, entry.duration);
      }
    }).observe({ type: 'longtask', buffered: true });
  } catch { /* no long-task timing here: the numbers stay zero */ }
  return {
    state, sweep,
    reset() { Object.assign(state, fresh()); },
    /** Indicative only: from a delivery to the first frame whose drawn rows
     * show every flip it made. Never a gate. */
    measureFlips(started) {
      const targets = [...document.querySelectorAll('.ah-session-list-item[data-standing="not-imported"]')]
        .map((row) => row.dataset.motionKey).filter((id) => current.sim.controller.streamState(id) !== 'pending');
      if (targets.length === 0) return;
      let frames = 0;
      const look = () => {
        frames += 1;
        const shown = targets.every((id) => document.querySelector(`.ah-session-list-item[data-motion-key="${id}"]`)?.dataset.standing !== 'not-imported');
        if (shown) state.flipPaint.push(performance.now() - started);
        else if (frames < 240) requestAnimationFrame(look);
      };
      requestAnimationFrame(look);
    },
    report() {
      sweep();
      return { violations: state.violations, faces: [...state.faces], cards: state.cards, flipPaint: state.flipPaint,
        longTasks: state.longTasks, everReady: state.everReady.size, selections: state.selections, opened: state.opened };
    },
  };
}

const probe = createProbe();

/** Every project row's top, each animation frame, until stopped: how the
 * rows travel, frame by frame (an end that animates moves them a little per
 * frame; one that jumps moves them all at once). */
function sampleRows() {
  const frames = [];
  let on = true;
  const read = () => Object.fromEntries([...document.querySelectorAll('.ah-project-list-item')]
    .map((row) => [row.dataset.motionKey, row.getBoundingClientRect().top]));
  const step = () => {
    if (!on) return;
    frames.push(read());
    requestAnimationFrame(step);
  };
  frames.push(read());
  requestAnimationFrame(step);
  return () => {
    on = false;
    frames.push(read());
    return frames;
  };
}

/* ── the app ───────────────────────────────────────────────────────────── */

let setScenario = () => {};
function Fixture() {
  const [scenario, set] = useState(null);
  setScenario = set;
  return scenario === null ? null : <App key={scenario.key} host={scenario.host} />;
}

let mounts = 0;
let sampler = null;
window.coldImport = {
  /** A fresh app over a fresh import. */
  mount(options) {
    current = null;
    flushSync(() => setScenario(null));
    probe.reset();
    const sim = createColdImport({ now: NOW, ...options });
    const host = hostOver(sim);
    current = { sim, host };
    mounts += 1;
    flushSync(() => setScenario({ host, key: mounts }));
    return { now: sim.controller.now, sessions: sim.controller.corpus.sessions.length };
  },
  get sim() { return current.sim; },
  get c() { return current.sim.controller; },
  /** Every scripted click goes through here first, so the probe can tell a
   * selection the reader made from one the data made. */
  select() {
    probe.state.selections += 1;
    probe.state.lastSelection = performance.now();
  },
  advanceTo(ms) {
    const started = performance.now();
    current.sim.controller.advanceTo(ms);
    probe.measureFlips(started);
  },
  /** The app's own keys: `/` opens the sheet, the primary key with K
   * toggles it (App.tsx). */
  sheet(open) {
    const isOpen = document.querySelector('.ah-sheet__field') !== null;
    if (open === isOpen) return;
    window.dispatchEvent(open
      ? new KeyboardEvent('keydown', { key: '/', bubbles: true })
      : new KeyboardEvent('keydown', { key: 'k', metaKey: true, ctrlKey: true, bubbles: true }));
  },
  routeChange: () => current.host.routeChange(),
  startSampling() { sampler = sampleRows(); },
  stopSampling() {
    const frames = sampler?.() ?? [];
    sampler = null;
    return frames;
  },
  report: () => probe.report(),
};

createRoot(document.getElementById('root')).render(<Fixture />);
