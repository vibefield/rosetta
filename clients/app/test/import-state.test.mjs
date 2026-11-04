// Newest trees first, readable as each one closes. While an import runs,
// the session list keeps a "Not imported yet" group at its foot, the
// transcript says why a session is still empty and never reads "Nothing
// readable yet" between a tree's flip and its resync, the import card counts
// what is ready, and a held list's pill counts what arrived. Every fact comes
// from fields the daemon already sends; nothing here changes the protocol.
//
// Run with TZ=UTC (as `npm test` is): the list's day heads are local days.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ListActivityNotice } from "../../room/src/components/ListActivityNotice.tsx";
import { IngestStatus } from "../../room/src/components/IngestStatus.tsx";
import { ProjectListItem } from "../../room/src/components/projects/ProjectListItem.tsx";
import { SessionList } from "../../room/src/components/sessions/SessionList.tsx";
import { getSessionStatus } from "../../room/src/components/sessions/session-display.ts";
import { TranscriptDocument } from "../../room/src/components/transcript/Document.tsx";
import { transcriptModel } from "../../room/src/components/transcript/document-model.ts";
import { buildThreadIndex } from "../../room/src/components/transcript/thread-index.ts";
import { createDemoPorts } from "../../room/src/fixtures/index.ts";
import { compareNotImported } from "../../room/src/lib/activity-order.ts";
import {
  activityPhase, phaseHeading, readySessions, readySuffix, resumedRun,
} from "../../room/src/lib/ingest-activity.ts";
import {
  arrivalsLabel, commandFailureLabel, holdsImport, importArrivals, importCourse, importStanding,
  isMetadataOnly, notImportedLast, parentIds, watchImport,
} from "../../room/src/lib/import-state.ts";
import { buildProjectViewModelFromSessions } from "../../room/src/lib/model.ts";
import { RoomProvider } from "../../room/src/room-context.tsx";
import { ApiRequestError } from "../../ts/client.ts";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const { createElement } = fromRoom("react");
const { renderToStaticMarkup } = fromRoom("react-dom/server");

const NOW = Date.UTC(2026, 9, 2, 12);
const HOUR = 3_600_000;

function session(id, overrides = {}) {
  return {
    id, native_session_id: id, runtime_id: "rt", project_id: "p", cwd: "/work",
    parent_session_id: null, delegation: null, root_session_id: id, role: "main",
    title: `Title ${id}`, summary: null, started_at: NOW - 2 * HOUR, updated_at: NOW - HOUR,
    activity: null, stream_state: "indexed", ...overrides,
  };
}
const active = (at) => ({ activity: { first_at: at - 60_000, last_at: at } });
const pending = (updatedAt, overrides = {}) => ({ stream_state: "pending", updated_at: updatedAt, ...overrides });
// What the metadata pass makes for a parent whose own transcript was not found.
const metadataOnly = (overrides = {}) => ({
  stream_state: "pending", cwd: null, title: null, summary: null, delegation: null, project_id: null, ...overrides,
});

/** Day heads and rows in drawn order: heads as text, rows as their ids. */
function drawn(markup) {
  return [...markup.matchAll(/<div class="ah-day"[^>]*>([^<]*)<\/div>|data-motion-key="([^"]+)"/g)]
    .map(([, head, id]) => head ?? id);
}

function renderList(sessions, { duringBackfill = true, parents = new Set() } = {}) {
  return renderToStaticMarkup(createElement(SessionList, {
    sessions, factsBySessionId: new Map(), getRuntimeKind: () => "claude-code",
    selectedSessionId: null, currentTimestamp: NOW, duringBackfill, pending: false,
    onSessionSelect: () => {}, parentIds: parents,
  }));
}

/* ── the session list ─────────────────────────────────────────────────── */

test("while importing, the sessions it has not reached are drawn last, newest source first, under one undated head", () => {
  // In the room's order (HistoryRoom's sort): days newest first, then the
  // sessions with no recorded activity, by id.
  const rows = [
    session("today", active(NOW - HOUR)),
    session("yesterday", active(NOW - 26 * HOUR)),
    session("orphan", metadataOnly()),
    session("p1", pending(NOW - 5 * HOUR)),
    session("p2", pending(NOW - 2 * HOUR)),
    session("p3", pending(NOW - 2 * HOUR)),
  ];
  const markup = renderList(rows, { parents: new Set(["orphan"]) });
  assert.deepEqual(drawn(markup), [
    "Today", "today", "Yesterday", "yesterday",
    // A row made only for its children is never reached, so it is not waiting.
    "Activity unknown", "orphan",
    "Not imported yet · 3", "p2", "p3", "p1",
  ]);
  assert.match(markup, /<div class="ah-day" data-not-imported="">Not imported yet · 3<\/div>/);
  // Its time slot says why there is no time; the name says it once.
  assert.match(markup, /aria-label="Title p1 · not imported yet"/);
  assert.match(markup, /data-motion-key="p1"[^>]*>(?:(?!<\/button>).)*<span class="ah-last">not imported yet<\/span>/);
  // Its title is its native id, and nothing about it cooks.
  assert.match(markup, /data-motion-key="orphan" data-role="main" aria-label="orphan · transcript not available · /);

  // Once the import is over there is no group: the room's own order stands.
  assert.deepEqual(drawn(renderList(rows, { duringBackfill: false, parents: new Set(["orphan"]) })), [
    "Today", "today", "Yesterday", "yesterday", "Activity unknown", "orphan", "p1", "p2", "p3",
  ]);
});

test("the group's order is the source's freshness, then identity; everything else keeps the room's order", () => {
  const rows = [session("z", pending(5)), session("b"), session("y", pending(9)), session("a"), session("x", pending(9))];
  const waiting = (row) => row.stream_state === "pending";
  assert.deepEqual(notImportedLast(rows, waiting, compareNotImported).map((row) => row.id), ["b", "a", "x", "y", "z"]);
  assert.deepEqual(notImportedLast(rows.filter((row) => !waiting(row)), waiting, compareNotImported).map((row) => row.id), ["b", "a"]);
});

test("a row is not imported yet only while the import runs; one with no transcript of its own is unavailable", () => {
  const child = session("child", { parent_session_id: "orphan", root_session_id: "orphan", role: "subagent" });
  const orphan = session("orphan", metadataOnly());
  assert.deepEqual([...parentIds([orphan, child])], ["orphan"]);
  assert.equal(isMetadataOnly(orphan, true), true);
  // Without children it is a real session whose head said nothing.
  assert.equal(isMetadataOnly(orphan, false), false);
  assert.equal(importStanding(orphan, false, false), null);
  assert.equal(importStanding(orphan, true, false), "not-imported");
  assert.equal(importStanding(orphan, true, true), "unavailable");
  assert.equal(importStanding(orphan, false, true), "unavailable");
  // Any metadata of its own, or an opened tree, and it is an ordinary row.
  assert.equal(isMetadataOnly({ ...orphan, cwd: "/work" }, true), false);
  assert.equal(isMetadataOnly({ ...orphan, stream_state: "indexed" }, true), false);
  assert.equal(importStanding(session("open"), true, false), null);
});

test("the row's label: not imported yet while importing, transcript not available for a metadata-only row", () => {
  const waiting = session("w", pending(1));
  // Waiting on a running import is no trouble: no hue (the row draws a hollow dot).
  assert.deepEqual(getSessionStatus(waiting, true), { tone: null, isLive: false, label: "not imported yet" });
  assert.equal(getSessionStatus(waiting, false).tone, "orange");
  assert.equal(getSessionStatus(waiting, false).label, "not yet indexed");
  assert.deepEqual(getSessionStatus(waiting, true, true), { tone: null, isLive: false, label: "transcript not available" });
  assert.equal(getSessionStatus(session("s", { stream_state: "streaming" }), true).label, "indexing");
});

/* ── the held list's pill ─────────────────────────────────────────────── */

const at = (id, isPending = false) => (isPending ? { id, day: null, pending: true } : { id, day: 1 });

test("the pill counts the held not-imported rows the new order draws as imported", () => {
  const held = [at("a"), at("b"), at("p1", true), at("p2", true)];
  // At the top of the list the first row is the anchor.
  assert.deepEqual(importArrivals(held, [at("a"), at("p1"), at("b"), at("p2", true)], []), { count: 1, older: true });
  assert.deepEqual(importArrivals(held, [at("p1"), at("a"), at("b"), at("p2", true)], []), { count: 1, older: false });
  // Scrolled to b: one lands above the reader, so they are not all older.
  assert.deepEqual(importArrivals(held, [at("a"), at("p1"), at("b"), at("p2")], ["b", "p1"]), { count: 2, older: false });
  assert.deepEqual(importArrivals(held, [at("a"), at("b"), at("p1"), at("p2")], ["b", "p1"]), { count: 2, older: true });
  // An anchor the new order no longer draws gives way to the next one.
  assert.deepEqual(importArrivals(held, [at("a"), at("b"), at("p1"), at("p2")], ["gone", "b"]), { count: 2, older: true });
  // Nothing imported: other changes keep the room's own words.
  assert.equal(importArrivals(held, [at("b"), at("a"), at("p1", true), at("p2", true)], []), null);
  assert.equal(importArrivals([at("a"), at("b")], [at("b"), at("a")], []), null);
  assert.equal(arrivalsLabel({ count: 1, older: true }), "1 older conversation imported");
  assert.equal(arrivalsLabel({ count: 1_204, older: false }), "1,204 conversations imported");
});

test("the pill says what arrived, and says older only when every arrival lands below", () => {
  const notice = (props) => renderToStaticMarkup(createElement(ListActivityNotice, { onShow: () => {}, noun: "sessions", ...props }));
  const older = notice({ pending: true, arrivals: { count: 3, older: true } });
  assert.match(older, />3 older conversations imported — show<\/button>/);
  // The live region names the kind of change, never the count: the count
  // moves with every tree the import opens, and each change would be read.
  assert.match(older, /role="status">Newly imported conversations are available in sessions\.<\/span>/);
  const one = notice({ pending: true, arrivals: { count: 1, older: false } });
  assert.match(one, />1 conversation imported — show<\/button>/);
  assert.equal(/role="status">([^<]*)</.exec(one)[1], /role="status">([^<]*)</.exec(older)[1]);
  assert.match(notice({ pending: true, arrivals: null }), />Show recent activity<\/button>/);
  assert.match(notice({ pending: true }), /role="status">New activity is available in sessions\.<\/span>/);
  // A reorder the import caused names the import, never activity.
  const reordered = renderToStaticMarkup(createElement(ListActivityNotice, { onShow: () => {}, noun: "projects", pending: true, importReorder: true }));
  assert.match(reordered, />Projects reordered by the import · show<\/button>/);
  assert.match(reordered, /role="status">Projects were reordered by the import\.<\/span>/);
  assert.match(renderToStaticMarkup(createElement(ListActivityNotice, { onShow: () => {}, noun: "projects", pending: true })), />Show recent activity<\/button>/);
  const idle = notice({ pending: false, arrivals: { count: 3, older: true } });
  assert.match(idle, /role="status"><\/span>/);
  assert.match(idle, /hidden="">Show recent activity<\/button>/);
});

/* ── the transcript ───────────────────────────────────────────────────── */

let ordinal = 0;
function prompt(sessionId, text) {
  ordinal += 1;
  return {
    timeline_id: `t${ordinal}`, session_id: sessionId, ordinal,
    group: { id: `g${ordinal}`, kind: "user_turn", derivation: "native" },
    response: null, block_index: null, native_id: null, parent_native_id: null,
    at: NOW - HOUR + ordinal, model: null, effort: null,
    body: { type: "user", text: { type: "inline", text }, attachments: [] },
    parent_tool_native_id: null,
    usage: { input: null, output: null, cache_read: null, cache_write: null, thinking: null },
    provenance: { agent_version: null, src_cursor: null, src_offset: null },
  };
}

function feedOf(rows, snapshotReady = true) {
  const nothing = () => undefined;
  return {
    rows, snapshotReady, error: null, retry: nothing, timelineMeta: null, status: "live",
    watermark: null, earlier: { kind: "none" }, earlierNotice: null, earlierAbove: null,
    landed: null, holdEarlier: nothing, revealEarlier: nothing, retryEarlier: nothing,
    dismissEarlierNotice: nothing, canLoadEarlier: false, loadEarlier: nothing,
    loadAnchor: async () => false,
  };
}

function renderTranscript(row, { rows = [], duringBackfill = true, sessions = [row], importCourse } = {}) {
  const nothing = () => undefined;
  return renderToStaticMarkup(createElement(RoomProvider, { ports: createDemoPorts(), options: { storage: null } },
    createElement(TranscriptDocument, {
      session: row, model: transcriptModel(rows, row, null, NOW), feed: feedOf(rows), duringBackfill, importCourse,
      index: buildThreadIndex(sessions), focus: null, highlightedTimelineId: null,
      onFocusConsumed: nothing, onReceipt: nothing, onOpenAttachment: nothing, onOpenPath: nothing,
      getRuntimeKind: () => "claude-code", homeDirectory: null, onPillPress: null,
    })));
}

test("a session the import has not reached says so, and promises nothing it cannot keep", () => {
  const markup = renderTranscript(session("w", pending(NOW)));
  assert.match(markup, /<strong>Not imported yet<\/strong><p>Your newest conversations import first\. This one opens as soon as the import reaches it\.<\/p>/);
  assert.doesNotMatch(markup, /Nothing readable yet|Indexing history|Live updates resume/);
  assert.match(markup, /class="ah-meter" data-indeterminate=""/);
});

test("the not-imported face promises only what the import's course keeps", () => {
  // A resumed build withholds every session it had not opened until it
  // publishes: no tree opens on the way.
  const resumed = renderTranscript(session("w", pending(NOW)), { importCourse: "resumed" });
  assert.match(resumed, /<strong>Not imported yet<\/strong><p>The import picked up where it stopped\. This one opens when the import finishes\.<\/p>/);
  assert.doesNotMatch(resumed, /as soon as the import reaches it/);
  // A paused or failed run reaches nothing until it continues, and nothing sweeps.
  const stopped = renderTranscript(session("w", pending(NOW)), { importCourse: "stopped" });
  assert.match(stopped, /<p>The import stopped before it reached this one\. It opens once the import continues\.<\/p>/);
  assert.doesNotMatch(stopped, /as soon as the import reaches it|ah-meter/);

  assert.equal(importCourse({ state: "running" }, false), "newest-first");
  assert.equal(importCourse(null, false), "newest-first");
  assert.equal(importCourse({ state: "running" }, true), "resumed");
  for (const state of ["paused", "pausing", "failed"]) assert.equal(importCourse({ state }, true), "stopped");
});

test("an opened session reads during the import, under the line that says live updates wait", () => {
  const open = session("o");
  const rows = [prompt("o", "hello")];
  const during = renderTranscript(open, { rows });
  assert.match(during, /<div class="tr-mark" role="note" data-import-banner="">Live updates resume when the import finishes<\/div>/);
  assert.match(during, /hello/);
  assert.doesNotMatch(renderTranscript(open, { rows, duringBackfill: false }), /data-import-banner/);
});

test("a metadata-only parent says its transcript was not found, above the conversations it started", () => {
  const orphan = session("orphan", metadataOnly());
  const child = session("child", { parent_session_id: "orphan", root_session_id: "orphan", role: "subagent", project_id: "p" });
  for (const duringBackfill of [true, false]) {
    const markup = renderTranscript(orphan, { duringBackfill, sessions: [orphan, child] });
    assert.match(markup, /data-transcript-unavailable="">Transcript not available · only the conversations it started<\/div>/);
    assert.doesNotMatch(markup, /Not imported yet|History not yet available|Live updates resume/);
  }
});

test("between a tree's flip and its resync, the empty Pending rows keep the not-imported face", () => {
  const empty = [];
  let watch = watchImport(null, "s", "pending", null);
  assert.equal(holdsImport(watch, "pending"), false);
  watch = watchImport(watch, "s", "pending", empty);
  assert.equal(watch.pendingEra, true);
  // The catalog announces the flip before the session socket resyncs.
  const flipped = watchImport(watch, "s", "indexed", empty);
  assert.equal(flipped, watch, "the same delivery is the same watch");
  assert.equal(holdsImport(flipped, "indexed"), true);
  // The resync clears the snapshot, then delivers the opened rows.
  const reset = watchImport(flipped, "s", "indexed", null);
  assert.equal(holdsImport(reset, "indexed"), false);
  assert.equal(holdsImport(watchImport(reset, "s", "indexed", [prompt("s", "now readable")]), "indexed"), false);
  // Both resync frames in one render: rows delivered after the flip stand,
  // so a session with truly nothing readable may say so.
  assert.equal(holdsImport(watchImport(flipped, "s", "indexed", []), "indexed"), false);
  // The resync landing first: its rows arrive while the catalog still says Pending.
  const early = watchImport(watch, "s", "pending", [prompt("s", "early")]);
  assert.equal(holdsImport(watchImport(early, "s", "indexed", early.delivered), "indexed"), false);
  // Another session starts over.
  assert.equal(holdsImport(watchImport(flipped, "other", "indexed", empty), "indexed"), false);
});

test("a full command asked for during the import says why it is not there yet", () => {
  assert.equal(commandFailureLabel(new ApiRequestError(503, "not_ready", "this conversation is still importing")),
    "available when imported · retry");
  assert.equal(commandFailureLabel(new ApiRequestError(503, "not_ready", "full commands are available after raw-ledger publication")),
    "still saving · retry");
  assert.equal(commandFailureLabel(new ApiRequestError(503, "not_ready", "still saving · retry")), "still saving · retry");
  // A not_ready no wait will cure is not "still saving".
  assert.equal(commandFailureLabel(new ApiRequestError(503, "not_ready", "this server has no raw-ledger command reader")),
    "couldn’t load · retry");
  assert.equal(commandFailureLabel(new ApiRequestError(404, "not_found", "no raw command source for this row")),
    "couldn’t load · retry");
  assert.equal(commandFailureLabel(new Error("network")), "couldn’t load · retry");
});

/* ── the import card and the project card ─────────────────────────────── */

const runtime = (overrides = {}) => ({ kind: "claude-code", runtime_id: "rt", label: null, state: "streaming",
  files_total: 100, files_skimmed: 100, sessions_total: 80, bytes_total: 1_000_000, bytes_streamed: 400_000,
  records: 300, ...overrides });
const project = (done, total = 40) => ({ project_id: "p", sessions_total: total, sessions_done: done,
  bytes_total: 1_000_000, bytes_streamed: 400_000 });
const feed = (ingest = {}, overrides = {}) => ({
  ingest: { phase: "backfilling", projects: [project(0)], runtimes: [runtime()], run: { id: "run" }, ...ingest },
  canonicalReady: false, searchReadiness: "building", searchProgress: null, connectionError: null,
  startedAt: 1, uptimeMs: 5_000, receivedAt: 1_000, ...overrides,
});

test("the import card counts the conversations ready to read, and drops the count at zero", () => {
  const twelve = activityPhase(feed({ projects: [project(5), { ...project(7), project_id: null }] }));
  assert.equal(twelve.title, "Importing your newest conversations first");
  assert.equal(phaseHeading(twelve), "Importing your newest conversations first · 12 ready");
  assert.equal(readySuffix(twelve), " · 12 ready");
  assert.equal(twelve.detail, "Older history fills in while you browse. Conversations you start now appear when the import finishes.");
  assert.equal(twelve.fraction, .4, "the meter is still the bytes read");
  const none = activityPhase(feed());
  assert.equal(phaseHeading(none), "Importing your newest conversations first");
  assert.equal(none.ready, null);
  assert.equal(phaseHeading(activityPhase(feed({ projects: [project(1_234, 2_000)] }))),
    "Importing your newest conversations first · 1,234 ready");
  assert.equal(readySessions(null), 0);
});

test("the import card draws the count after its title, outside what its live region reads", () => {
  const run = { id: "run", index_id: "ix", revision: 1, state: "running", phase: "capturing", created_at: 0,
    updated_at: 0, progress_epoch: "run:1", runtime_ids: ["rt"], allowed_actions: [], error: null };
  const draw = (projects, overrides = {}) => {
    const given = feed({ projects, run }, overrides);
    const activity = { phase: activityPhase(given), feed: given, footer: "Connected", elapsed: "5s elapsed" };
    return renderToStaticMarkup(createElement(RoomProvider, { ports: createDemoPorts(), options: { storage: null } },
      createElement(IngestStatus, { activity })));
  };
  const markup = draw([project(12)]);
  assert.match(markup, /aria-live="polite" aria-atomic="true">Importing your newest conversations first<span aria-hidden="true"> · 12 ready<\/span><\/h3>/);
  assert.match(markup, /role="progressbar" aria-label="Importing your newest conversations first · 12 ready"/);
  assert.match(draw([project(0)]), /aria-atomic="true">Importing your newest conversations first<\/h3>/);
  // An interrupted feed says so, and claims no count it cannot refresh.
  assert.match(draw([project(12)], { connectionError: "gone" }), /aria-atomic="true">Progress updates paused<\/h3>/);
});

test("finishing says conversations may pause while the archive is saved", () => {
  for (const phase of ["finalizing", "publishing"]) {
    const view = activityPhase(feed({ phase, runtimes: [runtime({ state: "done" })] }));
    assert.match(view.detail, /Conversations may pause for a few seconds while the archive is saved\.$/);
    assert.equal(view.ready, undefined);
  }
});

test("a resumed build counts nothing it cannot see: its board starts at zero while open conversations stay open", () => {
  const restoring = feed({ phase: "restoring", runtimes: [runtime({ records: 800 })] });
  const key = resumedRun(null, restoring);
  assert.equal(key, "1:run");
  const phase = activityPhase(restoring);
  assert.equal(phase.title, "Restoring saved progress");
  assert.match(phase.detail, /Conversations you can open now stay open\./);
  assert.doesNotMatch(phaseHeading(phase), /ready/);

  // The mark outlives the restoring phase, through the catch-up after it.
  const catchingUp = feed({ projects: [project(0)] });
  assert.equal(resumedRun(key, catchingUp), key);
  const resumed = activityPhase({ ...catchingUp, resumed: true });
  assert.equal(phaseHeading(resumed), "Finishing the import");
  assert.equal(resumed.detail, "Conversations you can open now stay open. The rest, and any you start now, appear when the import finishes.");
  assert.equal(resumed.ready, null);
  // A count the board does reach still stays out of a resumed card.
  assert.equal(phaseHeading(activityPhase({ ...feed({ projects: [project(9)] }), resumed: true })), "Finishing the import");

  // It ends with the import, and a new daemon start that never restored is fresh.
  assert.equal(resumedRun(key, feed({ phase: "live" })), null);
  assert.equal(resumedRun(key, feed({}, { startedAt: 2 })), null);
  assert.equal(resumedRun(null, catchingUp), null);
});

function renderProject(backfill, runtimeStates = ["streaming"]) {
  const row = { id: "p", name: "Project", root_paths: ["/work"], git_repo: null };
  const view = buildProjectViewModelFromSessions(row, [session("a", pending(1))], undefined, () => "claude-code", NOW, () => "#888");
  return renderToStaticMarkup(createElement(ProjectListItem, {
    project: view, maximumLifetimeCost: 0, isSelected: false, homeDirectory: null, currentTimestamp: NOW,
    ingestActive: true, runtimeStates, backfill, onProjectSelect: () => {},
  }));
}

test("the project card says how many of its conversations are ready, and keeps the byte meter's words before the first", () => {
  assert.match(renderProject(project(3, 10)), /Reading conversations · 3 of 10 ready/);
  assert.match(renderProject(project(1_234, 2_000)), /Reading conversations · 1,234 of 2,000 ready/);
  const before = renderProject(project(0, 10));
  assert.match(before, /Reading conversations · 40%/);
  assert.doesNotMatch(before, /0 of 10/);
  assert.match(renderProject(project(10, 10)), /Preparing activity summaries/);
});
