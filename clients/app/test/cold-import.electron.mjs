// A cold import, characterized (ux:M5): rosetta's app over the simulator in
// room/src/fixtures/cold-import.ts, stepped through simulated time while the
// room keeps its own timers. Every assertion pins what the room does TODAY
// and names what may change it:
// - `ux:X1` … `ux:X9`: the UX item expected to flip it; that item's commit
//   flips the assertion in the same commit.
// - `forever`: an honesty rule (the metrics catalog's I1) that must hold.
// - `unplanned`: today's behaviour that no planned item changes; a change
//   here must be meant, and its commit updates the line.
//
// This is a LOGIC gate. It asserts no timing threshold: waits only bound how
// long the room may take to show a state. The flip→paint, long-task and px
// numbers it prints are indicative, never gates (tools/ui-bench measures).
//
//   npm run test:cold-import -w @vibefield/rosetta-agent-history [-- --scale=0.25]
//
// `--scale` (or COLD_IMPORT_SCALE) sizes the archive: 1, the default, is the
// MacBook shape (8,612 sessions, 1,139 trees, 119 projects); CI runs 0.25.
// Any scale from 0.1 up keeps every behaviour this test needs (smaller ones
// fail a stated fixture precondition). Fixture data only: no daemon, archive
// or app profile is read.
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { app, BrowserWindow } from 'electron';

const root = fileURLToPath(new URL('..', import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), 'rosetta-cold-import-'));
const headlessCi = process.platform === 'linux' && Boolean(process.env.CI);
if (headlessCi) app.disableHardwareAcceleration();
app.setPath('userData', path.join(temporary, 'profile'));
app.on('window-all-closed', () => {});
const scale = Number(process.argv.find((arg) => arg.startsWith('--scale='))?.slice(8) ?? process.env.COLD_IMPORT_SCALE ?? 1);
const pulse = setInterval(() => {}, 100);
const deadline = setTimeout(() => { console.error('Cold import characterization timed out'); app.exit(1); }, 240_000);
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let win, server;
const errors = [];
const started = Date.now();

/* ── driving the page ─────────────────────────────────────────────────── */

const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, message, ...args) {
  const truthy = `(...a) => Boolean((${fn.toString()})(...a))`;
  for (let i = 0; i < 600; i++) {
    if (await win.webContents.executeJavaScript(`(${truthy})(...${JSON.stringify(args)})`)) return;
    await pause(25);
  }
  throw Error(`Timed out waiting for ${message}`);
}
/** Poll a page function returning { actual, expected } until they agree or
 * the room had its chance; the caller then asserts on what it last saw. */
async function agreed(fn, ...args) {
  let last;
  for (let i = 0; i < 160; i++) {
    last = await check(fn, ...args);
    try { assert.deepEqual(last.actual, last.expected); return last; } catch { await pause(25); }
  }
  return last;
}
const frames = (n = 3) => check((n) => new Promise((resolve) => {
  const step = (left) => (left === 0 ? resolve(true) : requestAnimationFrame(() => step(left - 1)));
  step(n);
}), n);
/** Let the room ask status after the last step and draw its answer. */
async function settle() {
  const asked = await check(() => window.coldImport.c.log.status.length);
  await until((n) => window.coldImport.c.log.status.length > n, 'a status ask after the step', asked);
  await frames(3);
}
/** Wait until the project rows stop moving (list motion has finished). */
async function rowsAtRest() {
  let last = null;
  for (let i = 0; i < 200; i++) {
    const now = JSON.stringify(await check(tops));
    if (now === last) return;
    last = now;
    await frames(2);
  }
  throw Error('Timed out waiting for the project rows to come to rest');
}
const advanceTo = (ms) => check((ms) => { window.coldImport.advanceTo(ms); return true; }, ms);
const advanceToPhase = (phase) => check((phase) => { window.coldImport.advanceTo(window.coldImport.c.phaseAt(phase)); return window.coldImport.c.clock; }, phase);
/** A scripted click: the one input that may change the selection (probe c3). */
const click = (selector) => check((selector) => {
  const element = document.querySelector(selector);
  if (!element) throw Error(`no ${selector}`);
  window.coldImport.select();
  element.click();
  return true;
}, selector);
const hover = (list, inside) => check((list, inside) => {
  const host = list === 'sessions'
    ? document.querySelector('.ah-list-view:has(> .ah-session-list)')
    : document.querySelector('.ah-list-view:has(.ah-project-list-item)');
  host.dispatchEvent(new PointerEvent(inside ? 'pointerover' : 'pointerout', {
    bubbles: true, pointerType: 'mouse', relatedTarget: inside ? null : document.body,
  }));
  return true;
}, list, inside);
async function type(query) {
  await check((query) => {
    const input = document.querySelector('.ah-sheet__field input');
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, query);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    return true;
  }, query);
}
const sheet = (open) => check((open) => { window.coldImport.sheet(open); return true; }, open);
/** The trail's last entry, to read what follows it. */
const lastSeq = () => check(() => window.coldImport.c.log.trail.at(-1)?.seq ?? 0);
/** What the room's first status ask after `since` answered to: a connection
 * change that reached it (`connection`), its poll's cadence after the parked
 * ask was answered (`cadence`), or a re-ask while that ask still waited
 * (`refresh`); null while it has not asked. */
const firstAskAfter = (since) => check((since) => {
  const after = window.coldImport.c.log.trail.filter((entry) => entry.seq > since);
  const ask = after.find((entry) => entry.what === 'status-ask');
  if (ask === undefined) return null;
  const before = after.filter((entry) => entry.seq < ask.seq).map((entry) => entry.what);
  return before.includes('connection-change') ? 'connection' : before.includes('status-answer') ? 'cadence' : 'refresh';
}, since);

/* ── the characterization ledger ──────────────────────────────────────── */

const ledger = [];
const why = (item) => item === 'forever'
  ? 'This is an honesty rule (metrics I1): it must hold forever.'
  : item === 'unplanned'
    ? 'No planned UX item changes this: if the change was meant, update this line in the same commit; otherwise the room changed under it.'
    : `If ${item} landed, flip this assertion in the same commit; otherwise the room changed under it.`;
/** Today's truth for one behaviour, named by what may change it. */
function today(item, label, actual, expected) {
  try {
    assert.deepEqual(actual, expected);
  } catch {
    throw new assert.AssertionError({
      message: `[${item}] ${label}\n  characterized (today): ${JSON.stringify(expected)}\n  observed now:          ${JSON.stringify(actual)}\n  ${why(item)}`,
      actual, expected, operator: 'deepStrictEqual',
    });
  }
  ledger.push({ item, label, observed: actual });
}

/** A wait that is itself a characterization: the room shows this state
 * today, and a timeout names the item rather than a bare wait. */
async function shows(item, label, fn, ...args) {
  try {
    await until(fn, label, ...args);
  } catch {
    throw new assert.AssertionError({
      message: `[${item}] ${label}: the room never showed it\n  ${why(item)}`,
      actual: false, expected: true, operator: 'strictEqual',
    });
  }
  ledger.push({ item, label, observed: true });
}

/* ── page-side readings (each runs in the page) ───────────────────────── */

/** The two list columns, found by what they hold, never by position. */
const column = (name) => [...document.querySelectorAll('.ah-col')].find((col) => (name === 'projects'
  ? col.querySelector(':scope > .ah-veilhost .ah-project-list-item') !== null
  : col.querySelector('.ah-session-list') !== null)) ?? null;
const projectOrder = () => [...document.querySelectorAll('.ah-project-list-item')].map((row) => row.dataset.motionKey);
const tops = () => Object.fromEntries([...document.querySelectorAll('.ah-project-list-item')]
  .map((row) => [row.dataset.motionKey, Math.round(row.getBoundingClientRect().top)]));
/** The room's project rule as it stands (ux:X1, decision D1):
 * activity-ranked projects first (newest recorded root activity); then the
 * unranked ones, while the import runs (no canonical readiness yet, or the
 * ingest not live) by their newest root's updated_at, else by id alone. */
const projectsByToday = () => {
  const meta = window.coldImport.c.meta();
  const status = window.coldImport.c.status();
  const importing = status.canonicalReady !== true || (status.ingest !== null && status.ingest.phase !== 'live');
  const rank = new Map(meta.projects.map((project) => [project.id, null]));
  const source = new Map();
  for (const session of meta.sessions) {
    const key = session.project_id ?? '__rosetta_unassigned__';
    if (!rank.has(key)) rank.set(key, null);
    if (session.parent_session_id !== null) continue;
    const at = session.activity?.last_at ?? null;
    if (at !== null) rank.set(key, Math.max(rank.get(key) ?? -Infinity, at));
    source.set(key, Math.max(source.get(key) ?? -Infinity, session.updated_at));
  }
  const actual = [...document.querySelectorAll('.ah-project-list-item')].map((row) => row.dataset.motionKey);
  const known = (x, y, a, b) => {
    if (x === null && y !== null) return 1;
    if (y === null && x !== null) return -1;
    return (x !== null && y !== null ? y - x : 0) || a.localeCompare(b);
  };
  const expected = [...rank.keys()].sort((a, b) => {
    const x = rank.get(a), y = rank.get(b);
    return x === null && y === null && importing ? known(source.get(a) ?? null, source.get(b) ?? null, a, b) : known(x, y, a, b);
  });
  return { actual, expected, ranked: [...rank.values()].filter((at) => at !== null).length, importing };
};
const rowLook = (element) => ({
  id: element.dataset.motionKey, standing: element.dataset.standing, stream: element.dataset.streamState,
  tone: element.querySelector('.ah-session-list-item__status')?.dataset.tone ?? null,
  last: element.querySelector('.ah-last')?.textContent ?? null,
  // The metadata line's last slot is the activity span while no figures landed.
  span: element.querySelector('.ah-session-list-item__metadata')?.lastElementChild?.textContent ?? null,
  cooking: element.hasAttribute('data-cooking'),
});
const sessionGroups = (look) => {
  const groups = [];
  for (const element of document.querySelector('.ah-session-list')?.children ?? []) {
    if (element.classList.contains('ah-day')) {
      groups.push({ head: element.textContent, notImported: element.hasAttribute('data-not-imported'),
        group: element.getAttribute('data-not-imported'), rows: [] });
    } else if (element.matches('.ah-session-list-item')) {
      groups.at(-1)?.rows.push(look(element));
    }
  }
  return groups;
};
const groups = () => win.webContents.executeJavaScript(`(${sessionGroups.toString()})((${rowLook.toString()}))`);
const card = () => {
  const section = document.querySelector('.ah-ingest-status');
  return section === null ? null : {
    state: section.dataset.state, ready: Number(section.dataset.readyCount), total: Number(section.dataset.readyTotal),
    title: section.querySelector('h3')?.textContent ?? null,
    detail: section.querySelector('.ah-ingest-status__detail')?.textContent ?? null,
    horizon: section.querySelector('.ah-ingest-status__horizon')?.textContent ?? null,
  };
};
/** The conversations the card counts, read from the catalog the room draws:
 * root sessions but metadata-only placeholders, and the readable among them
 * (not Pending); for one project when `projectKey` is given. */
const conversationsIn = (projectKey) => {
  const c = window.coldImport.c;
  const roots = c.meta().sessions.filter((s) => s.parent_session_id === null && c.kindOf(s.id) !== 'placeholder' &&
    (projectKey === undefined || (s.project_id ?? '__rosetta_unassigned__') === projectKey));
  const readable = roots.filter((s) => s.stream_state !== 'pending').length;
  const n = (v) => v.toLocaleString('en-US');
  // The recency spans run back from the reader's clock (the fixture's
  // Date.now is the simulated one), over each root's updated_at.
  const span = (days) => {
    const inside = roots.filter((s) => Date.now() - s.updated_at <= days * 86_400_000);
    return { readable: inside.filter((s) => s.stream_state !== 'pending').length, total: inside.length };
  };
  const day = span(1), week = span(7);
  const dayDone = day.total > 0 && day.readable >= day.total, weekDone = week.total > 0 && week.readable >= week.total;
  const horizon = day.total > 0 && !dayDone ? `Last 24 hours: ${n(day.readable)} of ${n(day.total)} readable`
    : week.total > 0 && !weekDone ? `${dayDone ? 'Last 24 hours ready ✓ · ' : ''}Last 7 days: ${n(week.readable)} of ${n(week.total)}`
    : weekDone ? 'Last 7 days ready ✓' : null;
  return { readable, total: roots.length,
    text: `${n(readable)} of ${n(roots.length)} ${roots.length === 1 ? 'conversation' : 'conversations'} readable`, horizon };
};
const transcript = () => {
  const column = document.querySelector('.ah-col--measure');
  const face = column?.querySelector('.ah-empty[data-import-face]');
  return {
    session: column?.querySelector('.tr-stage')?.dataset.sessionKey ?? null,
    face: face?.dataset.importFace ?? null, course: face?.dataset.importCourse ?? null,
    title: face?.querySelector('strong')?.textContent ?? null, why: face?.querySelector('p')?.textContent ?? null,
    meter: face?.querySelector('.ah-meter') !== null && face !== null && face !== undefined,
    sittings: column?.querySelectorAll('.tr-sitting').length ?? 0,
    banner: column?.querySelector('[data-import-banner]')?.textContent ?? null,
    bubble: column?.querySelector('header .ah-bubble')?.textContent ?? null,
    range: column?.querySelector('header .ah-caption')?.textContent ?? null,
  };
};
/** Root ids of a project, newest source first, with what the import does with them. */
const rootsOf = (projectKey) => window.coldImport.c.meta().sessions
  .filter((s) => s.parent_session_id === null && (s.project_id ?? '__rosetta_unassigned__') === projectKey)
  .map((s) => ({ id: s.id, updated: s.updated_at, kind: window.coldImport.c.kindOf(s.id), flip: window.coldImport.c.flipAt(s.id),
    bound: window.coldImport.c.treeOf(s.id)?.bound ?? false, dirty: window.coldImport.c.treeOf(s.id)?.dirty ?? false, stream: s.stream_state }));
/** The D1 split of a project's not-imported roots (ux:X2): newest source
 * first, those newer than every readable root's updated_at on top
 * ("Still importing"), the rest at the foot. Metadata-only placeholders are
 * in neither. */
const splitOf = (projectKey) => {
  const c = window.coldImport.c;
  const roots = c.meta().sessions.filter((s) => s.parent_session_id === null && (s.project_id ?? '__rosetta_unassigned__') === projectKey && c.kindOf(s.id) !== 'placeholder');
  const readable = roots.filter((s) => s.stream_state !== 'pending').map((s) => s.updated_at);
  const line = readable.length === 0 ? null : Math.max(...readable);
  const waiting = roots.filter((s) => s.stream_state === 'pending').sort((a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id));
  return { top: waiting.filter((s) => line === null || s.updated_at > line).map((s) => s.id), foot: waiting.filter((s) => line !== null && s.updated_at <= line).map((s) => s.id) };
};
/** A page function handed `column` as its first argument. */
const inColumn = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})((${column.toString()}), ...${JSON.stringify(args)})`);

/* ── the run ──────────────────────────────────────────────────────────── */

async function freshImport() {
  console.log(`Fresh import at scale ${scale}`);
  const mounted = await check((scale) => {
    const facts = window.coldImport.mount({ scale });
    window.coldImport.advanceTo(300); // the daemon's first walk has found the agents
    return facts;
  }, scale);
  console.log(`  ${mounted.sessions.toLocaleString()} metadata sessions in the simulated archive`);

  // ── The roll call → the import (ux:X7) ─────────────────────────────────
  // The roll call, the SDK and catalogOver run their real code (the fixture
  // mounts App on a RosettaClient), so a re-ask after the grant, wherever it
  // is wired, reaches the trail as a status ask. App wires it through the
  // room's handle (refreshStatus), so it reads as a refresh.
  await until(() => document.querySelector('[data-roll-call="ready"]'), 'the roll call');
  await check(() => { window.coldImport.c.holdStatus(); return true; });
  await until(() => window.coldImport.c.heldStatus() > 0, 'the room to park a status ask while consent waits');
  const beforeBegin = await lastSeq();
  await click('button[data-action="begin-ingest"]');
  await until((since) => window.coldImport.c.log.trail.some((e) => e.seq > since && e.what === 'grant'), 'the grant', beforeBegin);
  const grant = await check((since) => window.coldImport.c.log.trail.find((e) => e.seq > since && e.what === 'grant').seq, beforeBegin);
  // Its chance to react: a re-ask "at once" lands within these frames.
  await frames(3);
  today('ux:X7', 'once the grant resolves the room drops its parked ask and asks status once more, at once, with no connection change',
    await check((since) => {
      const after = window.coldImport.c.log.trail.filter((e) => e.seq > since).map((e) => e.what);
      return { asks: after.filter((what) => what === 'status-ask').length, aborts: after.filter((what) => what === 'status-abort').length,
        changes: after.filter((what) => what === 'connection-change').length };
    }, grant), { asks: 1, aborts: 1, changes: 0 });
  today('ux:X7', 'the roll call waits, "starting" and reading "Starting your import…", until a status answer moves the phase on',
    await check(() => ({ state: document.querySelector('[data-roll-call]')?.dataset.rollCall ?? null,
      button: document.querySelector('button[data-action="begin-ingest"]')?.textContent ?? null })),
    { state: 'starting', button: 'Starting your import…' });
  await check(() => { window.coldImport.advanceTo(window.coldImport.c.phaseAt('consent') + 200); window.coldImport.c.releaseStatus(); return true; });
  await shows('ux:X7', 'the face that follows consent arrives with the re-ask\'s answer, none earlier',
    () => document.querySelector('.ah-empty[data-face="cold-start"]'));
  await until((since) => window.coldImport.c.log.trail.some((e) => e.seq > since && e.what === 'status-ask'), 'the next status ask', grant);
  today('ux:X7', 'the first status ask after the grant is a refresh, while the parked ask still waited, not the poll\'s cadence or a connection change',
    await firstAskAfter(grant), 'refresh');
  // The instrument tells the seams apart: a host route change reaching the
  // room also re-asks at once, and reads as a connection change.
  await check(() => { window.coldImport.c.holdStatus(); return true; });
  await until(() => window.coldImport.c.heldStatus() > 0, 'a parked ask');
  const beforeRoute = await lastSeq();
  assert.ok(await check(() => window.coldImport.routeChange()) > 0, 'the host has route listeners (catalogOver\'s words)');
  await frames(3);
  assert.equal(await firstAskAfter(beforeRoute), 'connection', 'a route change re-asks at once and the trail calls it a connection change (the X7 seam is observable)');
  await check(() => { window.coldImport.c.releaseStatus(); return true; });

  // ── Metadata public ────────────────────────────────────────────────────
  await advanceToPhase('metadata');
  const projectCount = await check(() => new Set(window.coldImport.c.meta().sessions.map((s) => s.project_id ?? '__rosetta_unassigned__')).size);
  await until((n) => document.querySelectorAll('.ah-project-list-item').length === n, 'every project row', projectCount);
  await settle();
  const metaDelivery = await check(() => window.coldImport.c.log.meta.filter((m) => m.why === 'metadata').map((m) => m.source));
  console.log(`  metadata reached the room as: ${metaDelivery.join(', ')} (a burst over the hub's 1,024 queue is a resync + snapshot)`);
  const atMetadata = await agreed(projectsByToday);
  today('ux:X1', 'project order at metadata: every project unranked, so ordered by its newest root\'s updated_at, then id',
    { order: atMetadata.actual, ranked: atMetadata.ranked, importing: atMetadata.importing }, { order: atMetadata.expected, ranked: 0, importing: true });
  const newest = await check(() => {
    const roots = window.coldImport.c.meta().sessions.filter((s) => s.parent_session_id === null && window.coldImport.c.kindOf(s.id) === 'root');
    const giant = roots.sort((a, b) => b.updated_at - a.updated_at)[0];
    const key = giant.project_id ?? '__rosetta_unassigned__';
    return { id: giant.id, project: key, at: [...document.querySelectorAll('.ah-project-list-item')].findIndex((row) => row.dataset.motionKey === key) };
  });
  today('ux:X1', 'at metadata the project holding the newest conversation is drawn first', newest.at, 0);

  // The import card at metadata (ux:X3).
  const cardAtMetadata = await check(card);
  const atMetadataCount = await check(conversationsIn);
  today('ux:X3', 'the card while nothing is readable counts conversations from the catalog: "· 0 of N conversations readable"',
    { state: cardAtMetadata.state, ready: cardAtMetadata.ready, total: cardAtMetadata.total, title: cardAtMetadata.title },
    { state: 'reading', ready: 0, total: atMetadataCount.total, title: `Importing your newest conversations first · ${atMetadataCount.text}` });
  // The spans are anchored on the reader's clock (now): this fixture moves
  // that clock with the import, while a frozen corpus older than a week
  // shows no line at all (the room's unit tests pin that case).
  today('ux:X3', 'a recency line under the meter counts the last 24 hours by the reader\'s clock',
    { line: cardAtMetadata.horizon, starts: /^Last 24 hours: 0 of \d+ readable$/.test(cardAtMetadata.horizon ?? '') },
    { line: atMetadataCount.horizon, starts: true });
  today('unplanned', 'the reading card\'s detail sentence', cardAtMetadata.detail,
    'Older history fills in while you browse. Conversations you start now appear when the import finishes.');
  const unreached = await check((key) => {
    const streamed = window.coldImport.c.status().ingest.projects.find((row) => (row.project_id ?? '__rosetta_unassigned__') === key)?.bytes_streamed ?? 0;
    return { streamed, label: document.querySelector(`.ah-project-list-item[data-motion-key="${key}"] .ah-project-list-item__progress`)?.textContent ?? null };
  }, newest.project);
  const newestCount = await check(conversationsIn, newest.project);
  today('ux:X3', 'a project card with nothing readable says so in conversations: "Not reached yet" before its bytes, else "0 of n"',
    unreached.label, unreached.streamed === 0 ? 'Not reached yet' : newestCount.text);
  today('ux:X6', 'before any figure lands the projects head is the bare count, not the portfolio panel',
    await inColumn((column) => ({ portfolio: column('projects')?.querySelector('.ah-meta__body') !== null,
      head: column('projects')?.querySelector('.ah-meta__head .ah-eyebrow')?.textContent ?? null })), { portfolio: false, head: 'Projects' });
  today('ux:X6', 'the launcher counts the conversation titles the sheet searches: named roots, placeholders excluded',
    await check(() => document.querySelector('.ah-launcher__text')?.textContent), await check(() => {
      const c = window.coldImport.c;
      const titled = c.meta().sessions.filter((s) => s.parent_session_id === null && c.kindOf(s.id) !== 'placeholder' && (s.title ?? s.summary) !== null);
      return `Search ${titled.length.toLocaleString('en-US')} conversation titles`;
    }));

  const tokenSort = () => {
    const button = [...document.querySelectorAll('.ah-sortseg button')].find((b) => b.textContent === 'token');
    return button === undefined ? null : { disabled: button.disabled, title: button.title || null, pressed: button.getAttribute('aria-pressed') };
  };
  today('ux:X6', 'during the import the token sort waits, disabled, and says why',
    await check(tokenSort), { disabled: true, title: 'Spend ranks arrive after the import', pressed: 'false' });

  // The newest project's list at metadata (ux:X2).
  await click(`.ah-project-list-item[data-motion-key="${newest.project}"]`);
  await until(() => document.querySelector('.ah-session-list-item'), 'the newest project\'s sessions');
  const groupsAtMetadata = await groups();
  const expectedFoot = await check((key) => window.coldImport.c.meta().sessions
    .filter((s) => s.parent_session_id === null && (s.project_id ?? '__rosetta_unassigned__') === key && window.coldImport.c.kindOf(s.id) !== 'placeholder')
    .sort((a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id)).map((s) => s.id), newest.project);
  today('ux:X2', 'at metadata nothing is readable, so every row sits in one date-free "Still importing" group whose head counts its rows',
    groupsAtMetadata.map((group) => ({ head: group.head, group: group.group })), [{ head: `Still importing · ${expectedFoot.length.toLocaleString()}`, group: 'top' }]);
  today('ux:X2', 'the group orders by updated_at descending, then id',
    groupsAtMetadata[0].rows.map((row) => row.id), expectedFoot);
  const pendingLook = { standing: 'not-imported', stream: 'pending', tone: null, last: 'not imported yet', span: '—', cooking: false };
  today('ux:X2', 'a not-imported row: a hollow neutral dot (no tone), no shimmer, "not imported yet" in the time slot, "—" span',
    [...new Set(groupsAtMetadata[0].rows.map(({ id: _id, ...look }) => JSON.stringify(look)))].map((look) => JSON.parse(look)), [pendingLook]);
  today('ux:X6', 'the project panel\'s veil names the global gate during the import: "Charts appear when the whole import finishes"',
    await inColumn((column) => column('sessions')?.querySelector('.ah-meta__body .ah-veil')?.getAttribute('aria-label') ?? null), 'Charts appear when the whole import finishes');

  // Opening the newest (a giant tree, so it waits in the big lane) (ux:X5).
  await click(`.ah-session-list-item[data-motion-key="${newest.id}"]`);
  await until(() => document.querySelector('.ah-col--measure .ah-empty[data-import-face]') && document.querySelector('.tr-stage[data-ready]'), 'the not-imported face, revealed');
  today('ux:X5', 'a not-imported session opens on one promise, whatever its tree or course',
    await check(transcript), {
      session: newest.id, face: 'not-imported', course: 'newest-first', title: 'Not imported yet',
      why: 'Your newest conversations import first. This one opens as soon as the import reaches it.',
      meter: true, sittings: 0, banner: null, bubble: '0', range: 'Activity time unknown',
    });
  console.log(`  opening it asked boost ${await check((id) => window.coldImport.c.log.boosts.filter((b) => b.sessionId === id).length, newest.id)} time(s); the fresh RAM build attaches no boost queue`);

  // Titles stand in for full text before publication (ux:X6).
  const newestTitle = await check((id) => window.coldImport.c.meta().sessions.find((s) => s.id === id).title, newest.id);
  const topic = newestTitle.split(':')[0];
  await sheet(true);
  await until(() => document.querySelector('.ah-sheet__field input'), 'the search sheet');
  await type(newestTitle);
  await until(() => document.querySelector('.ah-sheet__row[data-hit-kind="title"]') && window.coldImport.c.log.search.length > 0 &&
    document.querySelector('.ah-sheet__count')?.textContent !== 'searching…', 'title matches and the answered search');
  today('ux:X6', 'before publication a not-imported title row says "Not imported yet"; the foot says titles only until the import finishes',
    await check((id) => ({
      when: document.getElementById(`ah-sheet-title-${id}`)?.querySelector('.ah-sheet__when')?.textContent ?? null,
      standing: document.getElementById(`ah-sheet-title-${id}`)?.dataset.standing ?? null,
      pending: document.querySelector('.ah-sheet__pending')?.textContent ?? null,
      foot: document.querySelector('.ah-sheet__count')?.textContent ?? null,
      fulltext: document.querySelectorAll('.ah-sheet__row[data-hit-kind="fulltext"]').length,
    }), newest.id), { when: 'Not imported yet', standing: 'not-imported', pending: 'Full-text search arrives when the import finishes.', foot: 'Titles only until the import finishes', fulltext: 0 });
  await sheet(false);
  await until(() => !document.querySelector('.ah-sheet__field'), 'the sheet to close');

  // ── Flips, mixed order, the inversion ─────────────────────────────────
  const roots = await check(rootsOf, newest.project);
  const giantFlip = roots.find((root) => root.id === newest.id).flip;
  const earlier = roots.filter((root) => root.kind === 'root' && root.flip !== null && root.flip < giantFlip && !root.bound);
  assert.ok(earlier.length > 0, 'fixture precondition: the newest project has an older tree that flips before its giant');
  const dirty = earlier.find((root) => root.dirty);
  assert.ok(dirty !== undefined, 'fixture precondition: one of them is a dirty tree, whose root flips Streaming');
  const inversionAt = Math.max(...earlier.map((root) => root.flip));
  await advanceTo(inversionAt);
  await until(() => [...document.querySelectorAll('.ah-session-list-item')].every((row) =>
    (window.coldImport.c.streamState(row.dataset.motionKey) !== 'pending') === (row.dataset.standing === 'ready')), 'rows to match the catalog');
  await settle();
  const inverted = await groups();
  const split = await check(splitOf, newest.project);
  assert.ok(split.foot.length > 0, 'fixture precondition: some not-imported root is older than the newest readable one');
  const drawnGroup = (name) => inverted.filter((group) => group.group === name);
  const countHead = (label, n) => (n === 0 ? [] : [`${label} · ${n.toLocaleString()}`]);
  today('ux:X2', 'mid-import (no inversion) the newest conversation waits on top in "Still importing", above the readable rows; older not-imported ones wait at the foot',
    { first: inverted[0]?.group, heads: inverted.filter((group) => group.notImported).map((group) => group.head),
      top: drawnGroup('top').flatMap((group) => group.rows.map((row) => row.id)), foot: drawnGroup('foot').flatMap((group) => group.rows.map((row) => row.id)),
      newestOnTop: drawnGroup('top').some((group) => group.rows.some((row) => row.id === newest.id)),
      readableBelowTop: inverted.slice(1).some((group) => group.rows.some((row) => row.standing === 'ready')), last: inverted.at(-1)?.group },
    { first: 'top', heads: [...countHead('Still importing', split.top.length), ...countHead('Not imported yet', split.foot.length)],
      top: split.top, foot: split.foot, newestOnTop: true, readableBelowTop: true, last: 'foot' });
  today('forever', 'unheld, no readable row is drawn under the "Not imported yet" head',
    inverted.filter((group) => group.notImported).flatMap((group) => group.rows).filter((row) => row.standing === 'ready').length, 0);
  const streamingRow = inverted.flatMap((group) => group.rows).find((row) => row.id === dirty.id);
  // Pending and Streaming no longer look alike: a Streaming root is
  // readable, so it keeps its orange "indexing" dot and figure shimmer and
  // shows its recorded time and span, where a not-imported row shows none.
  const dirtyActivity = await check((id) => window.coldImport.c.meta().sessions.find((s) => s.id === id).activity, dirty.id);
  today('ux:X2', 'a Streaming root (its source was unclean) reads ready: orange dot and shimmer, its recorded time and span, unlike a not-imported row',
    { standing: streamingRow?.standing, stream: streamingRow?.stream, tone: streamingRow?.tone, cooking: streamingRow?.cooking,
      timed: streamingRow?.last !== pendingLook.last && streamingRow?.last !== 'unknown', span: streamingRow?.span !== '—', activity: dirtyActivity !== null },
    { standing: 'ready', stream: 'streaming', tone: 'orange', cooking: true, timed: true, span: true, activity: true });
  const mixed = await agreed(projectsByToday);
  today('ux:X1', 'while ranked and unranked mix: activity-ranked projects first, the unranked after them by newest root updated_at',
    { order: mixed.actual, importing: mixed.importing }, { order: mixed.expected, importing: true });
  console.log(`  mixed order: ${mixed.ranked} of ${projectCount} projects ranked at ${(inversionAt / 1000).toFixed(1)} s`);

  // The count's unit (ux:X3).
  const unit = await check(() => {
    const status = window.coldImport.c.status();
    const sessionsDone = status.ingest.projects.reduce((sum, row) => sum + row.sessions_done, 0);
    const readableRoots = window.coldImport.c.meta().sessions.filter((s) => s.parent_session_id === null && s.stream_state !== 'pending').length;
    const shown = Number(document.querySelector('.ah-ingest-status')?.dataset.readyCount);
    const title = document.querySelector('.ah-ingest-status h3')?.textContent;
    return { shown, sessionsDone, readableRoots, title };
  });
  assert.ok(unit.sessionsDone > unit.readableRoots, 'fixture precondition: flipped children make the sessions count differ from the roots');
  const countUnit = unit.shown === unit.readableRoots ? 'roots' : unit.shown === unit.sessionsDone ? 'sessions' : 'other';
  const midCount = await check(conversationsIn);
  today('ux:X3', 'the count is readable conversations (roots, as the lists draw them), in the frame of the rows: "· k of N conversations readable"',
    { unit: countUnit, title: unit.title, horizon: (await check(card)).horizon },
    { unit: 'roots', title: `Importing your newest conversations first · ${midCount.text}`, horizon: midCount.horizon });
  console.log(`  "${midCount.text}" while the board counts ${unit.sessionsDone} sessions done`);
  today('ux:X3', 'a started project card counts its conversations: "k of n conversations readable"',
    await check((key) => document.querySelector(`.ah-project-list-item[data-motion-key="${key}"] .ah-project-list-item__progress`)?.textContent, newest.project),
    (await check(conversationsIn, newest.project)).text);

  // ── A held list while the giant flips under an open transcript ────────
  await hover('sessions', true);
  const beforeHold = await groups();
  const heldIds = beforeHold.filter((group) => group.notImported).flatMap((group) => group.rows.map((row) => row.id));
  const flatIds = (all) => all.flatMap((group) => group.rows.map((row) => row.id));
  const newestFrom = flatIds(beforeHold).indexOf(newest.id);
  await check(() => { window.coldImport.c.resyncMode = 'manual'; return true; });
  await advanceTo(giantFlip);
  await until((id) => document.querySelector(`.ah-session-list-item[data-motion-key="${id}"]`)?.dataset.standing === 'ready', 'the giant\'s row to read ready', newest.id);
  const arrivals = await check((ids) => ids.filter((id) => window.coldImport.c.streamState(id) !== 'pending').length, heldIds);
  await frames(3);
  const held = await groups();
  const heldGroup = held.find((group) => group.rows.some((row) => row.id === newest.id));
  const groupArrivals = await check((ids) => ids.filter((id) => window.coldImport.c.streamState(id) !== 'pending').length, heldGroup.rows.map((row) => row.id));
  const conversations = (n) => `${n} ${n === 1 ? 'conversation' : 'conversations'} imported`;
  today('ux:X4', 'held by the pointer: the held group counts the conversations imported (never "Activity updated") and the pill counts the arrivals',
    { head: heldGroup.head, group: heldGroup.group, activityUpdated: held.some((group) => group.head === 'Activity updated'),
      pill: await check(() => { const pill = document.querySelector('.ah-list-view:has(> .ah-session-list) > .ah-list-updates'); return pill.hidden ? null : pill.textContent; }) },
    { head: conversations(groupArrivals), group: 'top', activityUpdated: false, pill: `${conversations(arrivals)} — show` });
  today('forever', 'the catalog flipped the open session first: the face holds "Not imported yet" until rows arrive',
    await check(() => document.querySelector('.ah-col--measure .ah-empty[data-import-face]')?.dataset.importFace ?? null), 'not-imported');
  await check(() => window.coldImport.c.deliverResyncs());
  await shows('forever', 'its resync then reveals the rows, never "Nothing readable yet" in between',
    () => document.querySelector('.ah-col--measure .tr-sitting'));
  today('ux:X8', 'an open readable transcript during the import carries the generic live-updates banner',
    await check(() => document.querySelector('.ah-col--measure [data-import-banner]')?.textContent ?? null), 'Live updates resume when the import finishes');
  await hover('sessions', false);
  await until((id) => document.querySelector('.ah-session-list-item')?.dataset.motionKey === id, 'the applied order to put the newest first', newest.id);
  const applied = await groups();
  today('ux:X2', 'its flip moves the newest a short way: from its place on top in "Still importing" into the first day group, first',
    { from: newestFrom, to: flatIds(applied).indexOf(newest.id), into: applied[0]?.group ?? 'day' },
    { from: split.top.indexOf(newest.id), to: 0, into: 'day' });

  // The projects pill names the import when the import reorders them (ux:X4).
  // Step to the first flip that changes the rank rule's project order; the
  // schedule is deterministic, so that flip always comes.
  await agreed(projectsByToday);
  await hover('projects', true);
  const projectsHeld = await check(projectOrder);
  const flipTimes = await check(() => {
    const c = window.coldImport.c;
    const until = c.phaseAt('reading-done');
    return [...new Set(c.batches().filter((batch) => batch.kind === 'tree' && batch.at > c.clock && batch.at < until).map((batch) => batch.at))];
  });
  let reorderedAt = null;
  for (const at of flipTimes) {
    await advanceTo(at);
    const wanted = await check(projectsByToday);
    if (JSON.stringify(wanted.expected) !== JSON.stringify(projectsHeld)) { reorderedAt = at; break; }
  }
  today('ux:X4', 'an import flip reorders the held projects (by the rank rule) before reading ends', reorderedAt !== null, true);
  await shows('ux:X4', 'the reorder raises the projects pill while they are held',
    () => document.querySelector('.ah-list-view:has(.ah-project-list-item) > .ah-list-updates')?.hidden === false);
  today('ux:X4', 'held projects say "Projects reordered by the import · show" when the import reorders them, and hold their rows',
    { pill: await check(() => document.querySelector('.ah-list-view:has(.ah-project-list-item) > .ah-list-updates').textContent),
      held: JSON.stringify(await check(projectOrder)) === JSON.stringify(projectsHeld) },
    { pill: 'Projects reordered by the import · show', held: true });
  await hover('projects', false);

  // ── More flips under an open transcript, by how the resync lands ──────
  await check(() => { window.coldImport.c.resyncMode = 'immediate'; return true; });
  const nextPending = async (exclude) => check((exclude) => {
    const c = window.coldImport.c;
    const roots = c.meta().sessions.filter((s) => s.parent_session_id === null && c.kindOf(s.id) === 'root' &&
      s.stream_state === 'pending' && !c.treeOf(s.id).bound && !exclude.includes(s.id) && c.flipAt(s.id) > c.clock);
    roots.sort((a, b) => c.flipAt(a.id) - c.flipAt(b.id) || a.id.localeCompare(b.id));
    const pick = roots[0];
    return pick === undefined ? null : { id: pick.id, project: pick.project_id ?? '__rosetta_unassigned__', flip: c.flipAt(pick.id) };
  }, exclude);
  const openPending = async (target) => {
    await click(`.ah-project-list-item[data-motion-key="${target.project}"]`);
    await until((id) => document.querySelector(`.ah-session-list-item[data-motion-key="${id}"]`), 'the target\'s row', target.id);
    await click(`.ah-session-list-item[data-motion-key="${target.id}"]`);
    await until((id) => document.querySelector(`.tr-stage[data-session-key="${id}"][data-ready]`) && document.querySelector('.ah-col--measure .ah-empty[data-import-face="not-imported"]'), 'its not-imported face', target.id);
  };
  // The socket resyncs before the catalog lands (the daemon's own order).
  const second = await nextPending([newest.id]);
  await openPending(second);
  await advanceTo(second.flip);
  await shows('forever', 'a flip while open reveals the transcript when its socket resyncs first',
    () => document.querySelector('.ah-col--measure .tr-sitting') && !document.querySelector('.ah-col--measure .ah-empty[data-import-face]'));
  // The resync never arrives: the face holds, then asks again (3 s grace).
  const third = await nextPending([newest.id, second.id]);
  await openPending(third);
  await check(() => { window.coldImport.c.resyncMode = 'drop'; return true; });
  await advanceTo(third.flip);
  await check(() => { window.coldImport.c.resyncMode = 'immediate'; return true; });
  await frames(3);
  today('forever', 'a flip whose resync is lost keeps the not-imported face, on its one subscription, through the grace',
    await check((id) => ({
      face: document.querySelector('.ah-col--measure .ah-empty[data-import-face]')?.dataset.importFace ?? null,
      subscriptions: window.coldImport.c.log.subscribed.filter((s) => s.sessionId === id).length,
    }), third.id), { face: 'not-imported', subscriptions: 1 });
  await shows('forever', 'then the room subscribes again and shows the rows',
    (id) => document.querySelector('.ah-col--measure .tr-sitting') && window.coldImport.c.log.subscribed.filter((s) => s.sessionId === id).length >= 2, third.id);

  // A bound tree's root opens on the same promise and waits for the close (ux:X5).
  const bound = await check(() => {
    const c = window.coldImport.c;
    const row = c.meta().sessions.find((s) => s.parent_session_id === null && c.kindOf(s.id) === 'root' && c.treeOf(s.id).bound && s.stream_state === 'pending');
    return row === undefined ? null : { id: row.id, project: row.project_id ?? '__rosetta_unassigned__', flip: c.flipAt(row.id) };
  });
  await openPending(bound);
  today('ux:X5', 'a bound tree (opens only at its runtime\'s close) gets the same newest-first promise',
    await check(transcript).then((t) => ({ course: t.course, why: t.why })),
    { course: 'newest-first', why: 'Your newest conversations import first. This one opens as soon as the import reaches it.' });

  // ── Reading ends ──────────────────────────────────────────────────────
  await advanceToPhase('reading-done');
  await settle();
  today('ux:X3', 'when reading ends the card counts what is readable and says the rest open as their agent finishes',
    await check(card).then((c) => ({ state: c.state, title: c.title, detail: c.detail })),
    { state: 'organizing', title: `Opening conversations as each agent finishes · ${(await check(conversationsIn)).text}`,
      detail: 'Conversations still importing open as their agent finishes. Conversations you start now appear when the import finishes.' });
  await advanceToPhase('runtime-close');
  await shows('forever', 'the bound tree opens at its runtime\'s close, rows and all',
    () => document.querySelector('.ah-col--measure .tr-sitting') && !document.querySelector('.ah-col--measure .ah-empty[data-import-face]'));
  await advanceToPhase('finalizing');
  await settle();
  // A root no source rosters stays Pending past every close, so the count
  // stops short of its total and the card says so rather than "All N".
  const atFinalizing = await check(conversationsIn);
  assert.ok(atFinalizing.readable < atFinalizing.total, 'fixture precondition: never-rostered roots stay Pending');
  today('ux:X3', 'finalizing: step 3, "Finishing your archive" with the count, which stops short at never-rostered roots',
    await check(card).then((c) => ({ state: c.state, title: c.title, detail: c.detail })),
    { state: 'finalizing', title: `Finishing your archive · ${atFinalizing.text}`,
      detail: 'Finishing the archive so search and charts can start. Conversations may pause for a few seconds while the archive is saved.' });
  await advanceToPhase('publishing');
  await settle();
  today('unplanned', 'publishing, before canonical readiness: "Saving your archive"', await check(card).then((c) => c.title), 'Saving your archive');
  today('forever', 'no cross-session figure is asked for before canonical publication',
    await check(() => ({ stats: window.coldImport.c.log.projectStats.length, usage: window.coldImport.c.log.usage.length, day: window.coldImport.c.log.dayUsage.length })),
    { stats: 0, usage: 0, day: 0 });
  // Publication, measured from just before it to just after it settles.
  await rowsAtRest();
  const beforePublication = { rule: await agreed(projectsByToday), tops: await check(tops) };
  // The list as the reader sees it when figures may start: the selected
  // card, the cards inside the scrolled list, the rest top to bottom.
  const drawnAtPublication = await check(() => {
    const cards = [...document.querySelectorAll('.ah-project-list-item[data-motion-key]')].map((card) => {
      const box = card.getBoundingClientRect();
      const view = card.closest('.ah-scroll').getBoundingClientRect();
      return { id: card.dataset.motionKey, top: box.top, onScreen: box.bottom > view.top && box.top < view.bottom };
    }).sort((a, b) => a.top - b.top);
    return { cards, selected: document.querySelector('.ah-project-list-item[aria-current="true"]')?.dataset.motionKey ?? null };
  });

  // One project's figures fail from publication on: the last card drawn,
  // neither selected nor on screen.
  const failingProject = drawnAtPublication.cards.at(-1).id;
  assert.ok(failingProject !== drawnAtPublication.selected && !drawnAtPublication.cards.at(-1).onScreen,
    'fixture precondition: the last card is off screen and not selected');
  await check((id) => { window.coldImport.c.failFigures(id, true); return true; }, failingProject);

  // ── Canonical readiness, still publishing ─────────────────────────────
  await advanceToPhase('canonical');
  await settle();
  await until(() => window.coldImport.c.log.projectStats.length > 0, 'the first figures ask');
  today('ux:X9', 'canonical readiness during "publishing": the archive is saved, so the card moves on to search',
    { ...await check(card).then((c) => ({ state: c.state, title: c.title })), phase: await check(() => window.coldImport.c.status().ingest.phase) },
    { state: 'search_waiting', title: 'Preparing search', phase: 'publishing' });
  today('ux:X6', 'published but still "publishing": the panel veil keeps "Charts appear when the whole import finishes"',
    await inColumn((column) => column('sessions')?.querySelector('.ah-meta__body .ah-veil')?.getAttribute('aria-label') ?? null), 'Charts appear when the whole import finishes');
  await until(() => new Set(window.coldImport.c.log.projectStats.map((s) => s.projectId)).size >= document.querySelectorAll('.ah-project-list-item').length, 'every project\'s figures');
  // reveal_pending: the still-Pending rows' rollups, the end's one metadata change.
  await advanceToPhase('reveal');
  console.log(`  publication revealed ${await check(() => window.coldImport.c.log.meta.filter((m) => m.why === 'reveal').reduce((n, m) => n + m.rows, 0))} rows' activity (metadata-only placeholders and their ancestors)`);
  await advanceToPhase('live');
  await settle();
  await rowsAtRest();
  const afterPublication = { rule: await agreed(projectsByToday), tops: await check(tops) };
  const reranked = beforePublication.rule.actual.filter((id, i) => afterPublication.rule.actual[i] !== id).length;
  const ruleReranked = beforePublication.rule.expected.filter((id, i) => afterPublication.rule.expected[i] !== id).length;
  assert.ok(ruleReranked > 0, 'fixture precondition: publication\'s reveal moves a project (the archive guarantees it from scale 0.1 up)');
  today('unplanned', 'publication\'s reveal re-ranks projects by the activity it shows: the drawn order follows the rank rule',
    { order: afterPublication.rule.actual, reranked }, { order: afterPublication.rule.expected, reranked: ruleReranked });
  const kept = beforePublication.rule.actual.filter((id, i) => afterPublication.rule.actual[i] === id);
  const keptShifts = kept.map((id) => afterPublication.tops[id] - beforePublication.tops[id]);
  console.log(`  publication: ${reranked} of ${beforePublication.rule.actual.length} project rows changed position; the ${kept.length} that kept theirs moved ${[...new Set(keptShifts)].slice(0, 6).join(', ')} px`);
  today('unplanned', 'live, search not started: the card says "Preparing search" (the simulator counts documents after going live)',
    await check(card).then((c) => ({ state: c.state, title: c.title })), { state: 'search_waiting', title: 'Preparing search' });
  // The portfolio as the column draws it: its figures, and the card (if
  // any) right under its head.
  const portfolio = (column) => {
    const col = column('projects');
    return { panel: col?.querySelector(':scope > .ah-meta .ah-meta__body') !== null,
      cardUnderHead: col?.querySelector(':scope > .ah-meta + .ah-ingest-fold > * > .ah-ingest-status') !== null };
  };
  // Figures land on their own answers; give the room its chance to draw them.
  for (let i = 0; i < 200 && !(await inColumn(portfolio)).panel; i++) await frames(2);
  today('ux:X6', 'live, search not started: the portfolio panel is up, the card right under its head',
    await inColumn(portfolio), { panel: true, cardUnderHead: true });
  today('ux:X6', 'once the import publishes the token sort is offered again',
    await check(tokenSort), { disabled: false, title: null, pressed: 'false' });
  const askedFigures = await check((n) => window.coldImport.c.log.projectStats.map((s) => s.projectId).slice(0, n), drawnAtPublication.cards.length);
  const { cards: drawnCards, selected: selectedAtPublication } = drawnAtPublication;
  const onScreen = drawnCards.filter((card) => card.onScreen && card.id !== selectedAtPublication).map((card) => card.id);
  assert.ok(selectedAtPublication !== null && onScreen.length > 0 && onScreen.length + 1 < drawnCards.length,
    'fixture precondition: a selected project, and both on-screen and off-screen cards');
  today('ux:X6', 'figures hydrate the selected project first, then the on-screen cards top to bottom, then the rest as drawn',
    { order: askedFigures, selectedAt: askedFigures.indexOf(selectedAtPublication), onScreen: onScreen.length },
    { order: [selectedAtPublication, ...onScreen, ...drawnCards.map((card) => card.id).filter((id) => id !== selectedAtPublication && !onScreen.includes(id))],
      selectedAt: 0, onScreen: onScreen.length });
  console.log(`  figures: the selected project first, then ${onScreen.length} on-screen card(s), then ${drawnCards.length - 1 - onScreen.length} more`);

  // Search right after publication, the index still building (ux:X6).
  await advanceToPhase('search-indexing');
  // Part-way through, so the foot's % is the status's own non-zero number.
  await check(() => { const c = window.coldImport.c; window.coldImport.advanceTo((c.phaseAt('search-indexing') + c.phaseAt('search-optimizing')) / 2); return true; });
  await settle();
  await sheet(true);
  await until(() => document.querySelector('.ah-sheet__field input'), 'the search sheet after publication');
  await type(`${topic} `);
  await until(() => document.querySelector('.ah-sheet__row[data-hit-kind="title"]') && /being built · \d+%/.test(document.querySelector('.ah-sheet__count')?.textContent ?? ''), 'title rows with the building foot');
  const sheetAfter = await check(() => ({
    pending: document.querySelector('.ah-sheet__pending')?.textContent ?? null,
    foot: document.querySelector('.ah-sheet__count')?.textContent ?? '',
  }));
  const statusPct = await check(() => {
    const progress = window.coldImport.c.status().searchProgress;
    return Math.floor((100 * progress.documents_done) / progress.documents_total);
  });
  today('ux:X6', 'after publication, building: titles stand in, the note waits on the index, the foot gives the status\'s % and never "partial"',
    { pending: sheetAfter.pending, foot: sheetAfter.foot },
    { pending: 'Full-text search arrives when its index is built.', foot: `Full-text search is being built · ${statusPct}% · showing title matches` });
  await sheet(false);
  await until(() => !document.querySelector('.ah-sheet__field'), 'the sheet to close');
  today('ux:X6', 'while search builds the portfolio panel stays up, the card under its head',
    await inColumn(portfolio), { panel: true, cardUnderHead: true });
  // Every stage of the index build, as the card draws it.
  for (const stage of ['optimizing', 'validating', 'publishing']) {
    await advanceToPhase(`search-${stage}`);
    await settle();
  }

  // ── The end: one line, "Search ready ✓", then the card folds away ─────
  await rowsAtRest();
  await check(() => { window.coldImport.startSampling(); return true; });
  await advanceToPhase('search-ready');
  await shows('ux:X9', 'search ready: the card collapses in place to one line, "Search ready ✓", nothing else drawn',
    () => {
      const section = document.querySelector('.ah-ingest-status');
      return section?.dataset.state === 'complete' && section.hasAttribute('data-compact') &&
        section.querySelector('h3')?.textContent === 'Search ready ✓' &&
        !section.querySelector('.ah-ingest-status__detail, .ah-ingest-meter, .ah-ingest-status__eyebrow');
    });
  await rowsAtRest();
  const collapse = await check(() => window.coldImport.stopSampling());
  // I1's end state, as far as a fixture sees it: the board completed, the
  // count reached everything readable in the unit it counts (whichever that
  // is), and the open project draws every root the catalog has.
  const end = await check(() => {
    const c = window.coldImport.c;
    const status = c.status();
    const selected = document.querySelector('.ah-project-list-item[aria-current="true"]')?.dataset.motionKey;
    return {
      done: status.ingest.projects.reduce((n, row) => n + row.sessions_done, 0),
      total: status.ingest.projects.reduce((n, row) => n + row.sessions_total, 0),
      shown: Number(document.querySelector('.ah-ingest-status')?.dataset.readyCount),
      shownTotal: Number(document.querySelector('.ah-ingest-status')?.dataset.readyTotal),
      countedRoots: c.meta().sessions.filter((s) => s.parent_session_id === null && c.kindOf(s.id) !== 'placeholder').length,
      readableRoots: c.meta().sessions.filter((s) => s.parent_session_id === null && s.stream_state !== 'pending').length,
      drawn: document.querySelectorAll('.ah-session-list-item').length,
      roots: c.meta().sessions.filter((s) => s.parent_session_id === null && (s.project_id ?? '__rosetta_unassigned__') === selected).length,
    };
  });
  today('forever', 'I1 end state: the board counts every planned session done, the card\'s count reached all that is readable in its own unit, the open project draws all its roots',
    { boardComplete: end.done === end.total, countComplete: end.shown === (countUnit === 'roots' ? end.readableRoots : end.done), rows: end.drawn === end.roots },
    { boardComplete: true, countComplete: true, rows: true });
  today('ux:X3', 'the end count is in conversations: data-ready-count the readable roots, data-ready-total every root but placeholders',
    { count: end.shown === end.readableRoots, total: end.shownTotal === end.countedRoots }, { count: true, total: true });
  // The card's exit, frame by frame: its fold to nothing, under a head that
  // was already there.
  today('ux:X9', 'at ALL SET the portfolio already heads the column, the card under it: no head swap waits',
    await inColumn(portfolio), { panel: true, cardUnderHead: true });
  await check(() => { window.coldImport.startSampling(); return true; });
  await shows('ux:X9', 'five seconds after "Search ready" the card folds away and goes; the portfolio head stays',
    () => !document.querySelector('.ah-ingest-status') && !document.querySelector('.ah-ingest-fold') && [...document.querySelectorAll('.ah-col')].some((col) => col.querySelector(':scope > .ah-veilhost .ah-project-list-item') && col.querySelector('.ah-meta__body')));
  await frames(8);
  const exitSamples = await check(() => window.coldImport.stopSampling());
  /** How the project rows travelled over sampled frames. */
  const travel = (samples) => {
    const first = samples[0];
    const last = samples.at(-1);
    const ids = Object.keys(first).filter((id) => id in last);
    const shifts = ids.map((id) => Math.round(last[id] - first[id]));
    const steps = [];
    for (let i = 1; i < samples.length; i++) {
      const step = Math.max(0, ...ids.filter((id) => id in samples[i] && id in samples[i - 1]).map((id) => Math.abs(samples[i][id] - samples[i - 1][id])));
      if (step > 0.5) steps.push(Math.round(step));
    }
    const order = (sample) => Object.keys(sample).sort((a, b) => sample[a] - sample[b]).join();
    return { sameOrder: order(first) === order(last), uniform: Math.max(...shifts) - Math.min(...shifts) <= 1, shift: shifts[0] ?? 0,
      movingFrames: steps.length, maxStep: Math.max(0, ...steps), frames: samples.length };
  };
  // A logic bound, not a timing one: the move is spread over frames, so no
  // single frame carries more than half of it (an ease of a few hundred ms
  // keeps each step far below that at any frame rate this test sees).
  const glides = (move) => ({ sameOrder: move.sameOrder, uniform: move.uniform, moved: Math.abs(move.shift) > 1,
    spread: move.movingFrames > 1, largestStepAtMostHalf: move.maxStep <= Math.ceil(Math.abs(move.shift) / 2) });
  const calm = { sameOrder: true, uniform: true, moved: true, spread: true, largestStepAtMostHalf: true };
  const collapsed = travel(collapse);
  today('ux:X9', 'ALL SET\'s collapse to one line moves every project row by one common shift, spread over frames',
    glides(collapsed), calm);
  const exit = travel(exitSamples);
  today('ux:X9', 'the card\'s exit moves every project row by one common shift, spread over frames (its height eases to zero)',
    glides(exit), calm);
  console.log(`  card collapse: every row moved ${collapsed.shift} px; the largest one-frame step ${collapsed.maxStep} px over ${collapsed.movingFrames} moving frame(s) of ${collapsed.frames}`);
  // The failing project no longer veils the portfolio: the rest show, a
  // line says one is missing, and it is asked again until it lands.
  await until(() => document.querySelector('.ah-col .ah-meta__body [data-figures-failed]'), 'the failed-figures line');
  today('ux:X6', 'one project\'s failing figures: the portfolio unveils and says "figures for 1 project failed · retrying"',
    await inColumn((column) => {
      const body = column('projects')?.querySelector('.ah-meta__body');
      return { veil: body?.querySelector('.ah-veil') !== null, line: body?.querySelector('[data-figures-failed]')?.textContent ?? null };
    }), { veil: false, line: 'figures for 1 project failed · retrying' });
  const retried = await check((id) => window.coldImport.c.log.projectStats.filter((s) => s.projectId === id).length, failingProject);
  await check((id) => { window.coldImport.c.failFigures(id, false); return true; }, failingProject);
  // Retries back off (8 s, 16 s, … at most 2 min), so this wait is longer
  // than `until`'s; the retry timers are the room's own, in real time.
  const landedAt = Date.now();
  let landed = false;
  while (!landed && Date.now() - landedAt < 40_000) {
    landed = await check((id, n) => window.coldImport.c.log.projectStats.filter((s) => s.projectId === id).length > n &&
      !document.querySelector('.ah-col .ah-meta__body [data-figures-failed]'), failingProject, retried);
    if (!landed) await pause(100);
  }
  const reasked = await check((id, n) => window.coldImport.c.log.projectStats.slice(n).filter((s) => s.projectId !== id).length,
    failingProject, drawnAtPublication.cards.length);
  today('ux:X6', 'a failed project is asked again on its own (no other project re-asked), and once it answers the line goes',
    { landed, reasked }, { landed: true, reasked: 0 });
  console.log(`  the failed project was asked ${retried} time(s) before it answered; it landed ${((Date.now() - landedAt) / 1000).toFixed(1)} s after`);
  console.log(`  card exit: every row moved ${exit.shift} px; the largest one-frame step ${exit.maxStep} px over ${exit.movingFrames} moving frame(s) of ${exit.frames}`);
  const cards = (await check(() => window.coldImport.report())).cards;
  const tail = cards.map((c) => c.state).filter((state, i, all) => i === 0 || all[i - 1] !== state);
  today('unplanned', 'the card\'s end sequence, stepping through every phase and search stage',
    tail.slice(tail.indexOf('finalizing')),
    ['finalizing', 'publishing', 'search_waiting', 'search_indexing', 'search_optimizing', 'search_validating', 'search_publishing', 'complete', null]);

  // ── The honesty rules held throughout (metrics I1) ────────────────────
  const report = await check(() => window.coldImport.report());
  today('forever', 'I1: no row read ready before the catalog said so, none reverted, no selection moved without a click, no "Nothing readable yet"',
    report.violations, []);
  console.log(`  probe: ${report.everReady} rows seen ready, ${report.selections} scripted clicks, faces seen ${JSON.stringify(report.faces)}`);
  const paint = report.flipPaint.sort((a, b) => a - b);
  console.log(`  indicative only: flip→paint over ${paint.length} deliveries p50 ${Math.round(paint[Math.floor(paint.length / 2)] ?? 0)} ms, max ${Math.round(paint.at(-1) ?? 0)} ms; long tasks ${report.longTasks.count}, longest ${Math.round(report.longTasks.maxMs)} ms`);
  writeFileSync(path.join(temporary, 'fresh.png'), (await win.webContents.capturePage()).toPNG());
}

async function resumedImport() {
  console.log('Resumed import at scale 0.05');
  await check(() => { window.coldImport.mount({ scale: 0.05, course: 'resumed' }); return true; });
  const claude = await check(() => window.coldImport.c.runtimeAt(window.coldImport.c.corpus.runtimes[0].id));
  await advanceToPhase('restoring');
  await until(() => document.querySelector('.ah-ingest-status')?.dataset.state === 'restoring', 'the restoring card');
  // claude-code's metadata is public at its skim; its catch-up streams.
  await advanceTo(claude.metaPublic + 1_000);
  await until(() => document.querySelector('.ah-project-list-item'), 'projects of a resumed build');
  await settle();
  // The count is the catalog's, so it claims only what a reader can open:
  // nothing yet across launches, and what stayed open after a pause.
  today('ux:X3', 'a resumed build, seen restoring: "Finishing the import · k of N conversations readable", k from the catalog',
    await check(card).then((c) => ({ state: c.state, title: c.title, ready: c.ready })),
    await check(conversationsIn).then((n) => ({ state: 'reading', title: `Finishing the import · ${n.text}`, ready: n.readable })));
  const target = await check(() => {
    const c = window.coldImport.c;
    const row = c.meta().sessions.filter((s) => s.parent_session_id === null && c.kindOf(s.id) === 'root').sort((a, b) => b.updated_at - a.updated_at)[0];
    return { id: row.id, project: row.project_id ?? '__rosetta_unassigned__' };
  });
  await click(`.ah-project-list-item[data-motion-key="${target.project}"]`);
  await until((id) => document.querySelector(`.ah-session-list-item[data-motion-key="${id}"]`), 'the resumed newest row', target.id);
  await click(`.ah-session-list-item[data-motion-key="${target.id}"]`);
  await until(() => document.querySelector('.ah-col--measure .ah-empty[data-import-face]') && document.querySelector('.tr-stage[data-ready]'), 'the resumed face');
  today('ux:X5', 'resumed (caught restoring): the face promises the end of the import',
    await check(transcript).then((t) => ({ course: t.course, why: t.why, meter: t.meter })),
    { course: 'resumed', why: 'The import picked up where it stopped. This one opens when the import finishes.', meter: true });
  await check(() => { window.coldImport.c.setRunState('paused'); return true; });
  await settle();
  today('unplanned', 'stopped: the face says the import stopped, and no meter sweeps',
    await check(transcript).then((t) => ({ course: t.course, why: t.why, meter: t.meter })),
    { course: 'stopped', why: 'The import stopped before it reached this one. It opens once the import continues.', meter: false });
  await check(() => { window.coldImport.c.setRunState('running'); return true; });
  // Its runtime's end resyncs the open session, which still reads empty.
  await advanceTo(claude.close);
  await until((id) => window.coldImport.c.log.resyncs.some((r) => r.sessionId === id && r.delivered), 'the runtime\'s resync', target.id);
  await frames(3);
  today('forever', 'a resumed runtime\'s end resyncs the open session, still empty: the face stays, never "Nothing readable yet"',
    await check(() => document.querySelector('.ah-col--measure .ah-empty[data-import-face]')?.dataset.importFace ?? null), 'not-imported');
  // Nothing opens before publication: canonical readiness alone changes nothing.
  await advanceToPhase('canonical');
  await settle();
  today('forever', 'a resume opens nothing before its publication: at canonical readiness the catalog is all Pending and the face stays',
    await check(() => ({
      canonical: window.coldImport.c.status().canonicalReady,
      readable: window.coldImport.c.meta().sessions.filter((s) => s.stream_state !== 'pending').length,
      face: document.querySelector('.ah-col--measure .ah-empty[data-import-face]')?.dataset.importFace ?? null,
    })), { canonical: true, readable: 0, face: 'not-imported' });
  await advanceToPhase('reveal');
  await shows('forever', 'the resume\'s publication (one metadata diff, then the resync) opens the waiting transcript',
    () => document.querySelector('.ah-col--measure .tr-sitting') && !document.querySelector('.ah-col--measure .ah-empty[data-import-face]'));
  const report = await check(() => window.coldImport.report());
  today('forever', 'the waiting transcript first drew rows after canonical readiness',
    report.opened[target.id] >= await check(() => window.coldImport.c.phaseAt('canonical')), true);
  today('forever', 'I1 in the resumed build', report.violations, []);
}

async function run() {
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import('vite');
    server = await createServer({ configFile: path.join(root, 'vite.config.ts'), root, logLevel: 'error',
      server: { host: '127.0.0.1', port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    // The production window (electron/main.mjs: 1500×940).
    win = new BrowserWindow({ width: 1500, height: 940, show: headlessCi,
      webPreferences: { backgroundThrottling: false, contextIsolation: true, nodeIntegration: false } });
    win.webContents.on('console-message', (event) => { if (event.level === 'error') errors.push(event.message); });
    win.webContents.on('render-process-gone', (_event, details) => errors.push(JSON.stringify(details)));
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/cold-import.html`);
    await until(() => window.coldImport, 'the fixture');
    await freshImport();
    await resumedImport();
    assert.deepEqual(errors, []);
    console.log('\nCharacterized today (each flips with the item named, holds forever, or changes only on purpose):');
    // Ids are synthetic but long: a list of them prints as its length.
    const brief = (value) => JSON.stringify(value, (_key, v) =>
      Array.isArray(v) && v.length > 0 && v.every((x) => typeof x === 'string' && /^[0-9a-f]{64}$/.test(x)) ? `${v.length} ids`
        : typeof v === 'string' && /^[0-9a-f]{64}$/.test(v) ? 'id' : v).slice(0, 160);
    for (const entry of ledger) console.log(`  [${entry.item}] ${entry.label} — ${brief(entry.observed)}`);
    console.log(`\nPASS: ${ledger.length} characterizations at scale ${scale} in ${((Date.now() - started) / 1000).toFixed(1)} s. Screenshot: ${temporary}/fresh.png`);
  } catch (error) {
    console.error(error, ...errors);
    process.exitCode = 1;
  } finally {
    clearTimeout(deadline);
    await server?.close();
    app.exit(process.exitCode ?? 0);
  }
}
run();
