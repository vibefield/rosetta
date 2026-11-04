// Newest trees first, readable as each one closes. While an import runs,
// the session list keeps a "Not imported yet" group at its foot, the
// transcript says why a session is still empty and never reads "Nothing
// readable yet" between a tree's flip and its resync, the import card counts
// what is ready, and a held list's pill counts what arrived. Every fact comes
// from fields the daemon sends, the runtime's admission course among them.
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
  activityPhase, horizonLine, phaseHeading, readyConversations, resumedRun,
} from "../../room/src/lib/ingest-activity.ts";
import { ConversationFold } from "../../room/src/lib/conversation-readiness.ts";
import {
  arrivalsLabel, commandFailureLabel, holdsImport, importArrivals, importCourse, importStanding,
  isMetadataOnly, newestReadableSource, notImportedAround, notImportedLast, parentIds, watchImport,
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

test("while importing, the sessions it has not reached are drawn in two undated groups, newest source first", () => {
  // In the room's order (HistoryRoom's sort): days newest first, then the
  // sessions with no recorded activity, by id.
  const rows = [
    session("today", active(NOW - HOUR)),
    session("yesterday", active(NOW - 26 * HOUR)),
    session("orphan", metadataOnly()),
    session("p1", pending(NOW - 5 * HOUR)),
    session("p2", pending(NOW - 2 * HOUR)),
    session("p3", pending(NOW - 2 * HOUR)),
    // Newer than every readable row's source (NOW - HOUR): drawn on top.
    session("p0", pending(NOW - 10 * 60_000)),
  ];
  const markup = renderList(rows, { parents: new Set(["orphan"]) });
  assert.deepEqual(drawn(markup), [
    "Still importing · 1", "p0",
    "Today", "today", "Yesterday", "yesterday",
    // A row made only for its children is never reached, so it is not waiting.
    "Activity unknown", "orphan",
    "Not imported yet · 3", "p2", "p3", "p1",
  ]);
  assert.match(markup, /<div class="ah-day" data-not-imported="foot">Not imported yet · 3<\/div>/);
  assert.match(markup, /<div class="ah-day" data-not-imported="top">Still importing · 1<\/div>/);
  // Its time slot says why there is no time; the name says it once.
  assert.match(markup, /aria-label="Title p1 · not imported yet"/);
  assert.match(markup, /data-motion-key="p1"[^>]*>(?:(?!<\/button>).)*<span class="ah-last">not imported yet<\/span>/);
  // Its title is its native id, and nothing about it cooks.
  assert.match(markup, /data-motion-key="orphan" data-role="main" aria-label="orphan · transcript not available · /);

  // Once the import is over there is no group: the room's own order stands.
  assert.deepEqual(drawn(renderList(rows, { duringBackfill: false, parents: new Set(["orphan"]) })), [
    "Today", "today", "Yesterday", "yesterday", "Activity unknown", "orphan", "p1", "p2", "p3", "p0",
  ]);
  // Before anything is readable, everything is still importing.
  assert.deepEqual(drawn(renderList([session("p1", pending(1)), session("p2", pending(2))])), ["Still importing · 2", "p2", "p1"]);
});

test("the group's order is the source's freshness, then identity; everything else keeps the room's order", () => {
  const rows = [session("z", pending(5)), session("b"), session("y", pending(9)), session("a"), session("x", pending(9))];
  const waiting = (row) => row.stream_state === "pending";
  assert.deepEqual(notImportedLast(rows, waiting, compareNotImported).map((row) => row.id), ["b", "a", "x", "y", "z"]);
  assert.deepEqual(notImportedLast(rows.filter((row) => !waiting(row)), waiting, compareNotImported).map((row) => row.id), ["b", "a"]);
  // The top group: the waiting rows `above` picks, before everything else.
  assert.deepEqual(notImportedAround(rows, waiting, (row) => row.updated_at > 6, compareNotImported).map((row) => row.id), ["x", "y", "b", "a", "z"]);
  // Its line is the newest readable source; none readable, none drawn.
  assert.equal(newestReadableSource(rows, (row) => !waiting(row)), NOW - HOUR);
  assert.equal(newestReadableSource(rows, () => false), null);
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

function renderTranscript(row, { rows = [], duringBackfill = true, sessions = [row], importCourse, importAgent } = {}) {
  const nothing = () => undefined;
  return renderToStaticMarkup(createElement(RoomProvider, { ports: createDemoPorts(), options: { storage: null } },
    createElement(TranscriptDocument, {
      session: row, model: transcriptModel(rows, row, null, NOW), feed: feedOf(rows), duringBackfill, importCourse, importAgent,
      index: buildThreadIndex(sessions), focus: null, highlightedTimelineId: null,
      onFocusConsumed: nothing, onReceipt: nothing, onOpenAttachment: nothing, onOpenPath: nothing,
      getRuntimeKind: () => "claude-code", homeDirectory: null, onPillPress: null,
    })));
}

test("a session the import has not reached says so, and promises nothing it cannot keep", () => {
  // Per tree, a tree may still wait for its runtime's close (bound): no time
  // is promised, and no order or position is claimed.
  const markup = renderTranscript(session("w", pending(NOW)));
  assert.match(markup, /<strong>Not imported yet<\/strong><p>Newer conversations import first\. This one opens here once it is imported\.<\/p>/);
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

  // A runtime admitted whole opens each of its sessions at its close.
  const whole = renderTranscript(session("w", pending(NOW)), { importCourse: "runtime-close", importAgent: "Codex" });
  assert.match(whole, /<p>This one opens when Codex finishes importing\.<\/p>/);
  assert.match(whole, /data-import-course="runtime-close"/);

  // The course is the daemon's, per runtime; an older daemon's (none) falls
  // back to the room's inference of a resume.
  const ingest = (runtimes, state = "running") => ({ phase: "backfilling", run: { state }, projects: [], runtimes });
  const row = (runtime_id, admission_course) => ({ ...runtime({ runtime_id }), admission_course });
  const both = ingest([row("rt-claude", "per_tree"), row("rt-gemini", "at_runtime_close")]);
  assert.equal(importCourse(both, "rt-claude", false), "newest-first");
  assert.equal(importCourse(both, "rt-gemini", false), "runtime-close");
  assert.equal(importCourse(both, "rt-claude", true), "newest-first", "the daemon's course outranks the inference");
  assert.equal(importCourse(ingest([row("rt", "at_publication")]), "rt", false), "resumed",
    "a resume says so itself, seen restoring or not");
  assert.equal(importCourse(ingest([row("rt", undefined)]), "rt", false), "newest-first");
  assert.equal(importCourse(ingest([row("rt", null)]), "rt", true), "resumed");
  assert.equal(importCourse(null, null, false), "newest-first");
  assert.equal(importCourse(both, "elsewhere", true), "resumed");
  for (const state of ["paused", "pausing", "failed"]) {
    assert.equal(importCourse(ingest([row("rt", "at_publication")], state), "rt", true), "stopped");
  }
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

/** The room's fold over `rows` at NOW, as HistoryRoom hands it to the card. */
const counted = (rows, now = NOW) => new ConversationFold().read(rows, now);
/** `readable` of `total` conversations, all an hour old. */
const conversations = (readable, total) => counted(Array.from({ length: total }, (_, i) =>
  session(`c${i}`, i < readable ? {} : pending(NOW - HOUR))));

test("the import card counts readable conversations from the catalog, zero included", () => {
  const twelve = activityPhase(feed({ projects: [project(5), { ...project(7), project_id: null }] }, { conversations: conversations(12, 40) }));
  assert.equal(twelve.title, "Importing your newest conversations first");
  assert.equal(phaseHeading(twelve), "Importing your newest conversations first · 12 of 40 conversations readable");
  assert.equal(twelve.count, " · 12 of 40 conversations readable");
  assert.equal(twelve.detail, "Older history fills in while you browse. Conversations you start now appear when the import finishes.");
  assert.equal(twelve.fraction, .4, "the meter is still the bytes read");
  // Zero is a count the catalog states, not a guess.
  assert.equal(phaseHeading(activityPhase(feed({}, { conversations: conversations(0, 1_139) }))),
    "Importing your newest conversations first · 0 of 1,139 conversations readable");
  // Before the catalog names any conversation there is nothing to count.
  assert.equal(phaseHeading(activityPhase(feed())), "Importing your newest conversations first");
  assert.deepEqual(readyConversations(feed()), { readable: 0, total: 0 });
  assert.deepEqual(readyConversations(feed({}, { conversations: conversations(3, 7) })), { readable: 3, total: 7 });
});

test("the fold counts roots, never children or metadata-only placeholders, and follows every delivery", () => {
  const fold = new ConversationFold();
  const root = session("r");
  const child = session("k", { parent_session_id: "r", root_session_id: "r" });
  const placeholder = session("ph", metadataOnly());
  const orphan = session("o", { parent_session_id: "ph", root_session_id: "ph" });
  const waiting = session("w", pending(NOW - 2 * HOUR, { project_id: "q" }));
  const first = [root, child, placeholder, orphan, waiting];
  const at = fold.read(first, NOW);
  assert.deepEqual([at.readable, at.total], [1, 2]);
  assert.deepEqual(at.byProject.get("p"), { readable: 1, total: 1 });
  assert.deepEqual(at.byProject.get("q"), { readable: 0, total: 1 });
  assert.equal(fold.read(first, NOW), at, "the same delivery is the same answer");
  // A flip, a removal and a placeholder's last child leaving: each lands.
  const flipped = { ...waiting, stream_state: "streaming" };
  const second = fold.read([root, child, placeholder, flipped], NOW);
  assert.deepEqual([second.readable, second.total], [2, 3], "without children the placeholder row is an ordinary Pending root");
  // And back again, as a render of an older delivery would ask.
  const back = fold.read(first, NOW);
  assert.deepEqual([back.readable, back.total, back.byProject.get("q")], [1, 2, { readable: 0, total: 1 }]);
  // The fold agrees with a fresh count of the same rows.
  const fresh = counted([root, child, placeholder, flipped]);
  assert.deepEqual([fresh.readable, fresh.total], [second.readable, second.total]);
});

test("the recency line runs back from the reader's clock: the last 24 hours, then the last 7 days", () => {
  const DAY = 24 * HOUR;
  const rows = (readableDay, day, readableWeek, week) => [
    ...Array.from({ length: day }, (_, i) => session(`d${i}`, i < readableDay ? { updated_at: NOW - HOUR } : pending(NOW - HOUR))),
    ...Array.from({ length: week }, (_, i) => session(`w${i}`, i < readableWeek ? { updated_at: NOW - 3 * DAY } : pending(NOW - 3 * DAY))),
    session("old", pending(NOW - 30 * DAY)),
  ];
  const line = (...shape) => horizonLine(counted(rows(...shape)).horizons);
  assert.deepEqual(line(18, 24, 0, 1_031), { line: "Last 24 hours: 18 of 24 readable", reached: null });
  assert.deepEqual(line(24, 24, 788, 1_031), { line: "Last 24 hours ready ✓ · Last 7 days: 812 of 1,055", reached: "Last 24 hours ready" });
  assert.deepEqual(line(24, 24, 1_031, 1_031), { line: "Last 7 days ready ✓", reached: "Last 7 days ready" });
  assert.deepEqual(line(0, 0, 2, 5), { line: "Last 7 days: 2 of 5", reached: null }, "an empty day skips to the week");
  // A frozen archive older than a week, by the reader's clock: no line.
  assert.equal(line(0, 0, 0, 0), null);
  assert.equal(horizonLine(counted(rows(1, 2, 0, 0), NOW + 60 * DAY).horizons), null);
  // A fresh build draws it under the meter; a resumed one, which opens nothing
  // new before it publishes, does not.
  const fresh = activityPhase(feed({}, { conversations: counted(rows(18, 24, 0, 1_031)) }));
  assert.equal(fresh.horizon?.line, "Last 24 hours: 18 of 24 readable");
  assert.equal(activityPhase(feed({}, { conversations: counted(rows(18, 24, 0, 1_031)), resumed: true })).horizon, null);
});

test("the import card draws its count after the title, and its live regions say only the step and each span reached", () => {
  const run = { id: "run", index_id: "ix", revision: 1, state: "running", phase: "capturing", created_at: 0,
    updated_at: 0, progress_epoch: "run:1", runtime_ids: ["rt"], allowed_actions: [], error: null };
  const draw = (counts, overrides = {}) => {
    const given = feed({ projects: [project(12)], run }, { conversations: counts, ...overrides });
    const activity = { phase: activityPhase(given), feed: given, footer: "Connected", elapsed: "5s elapsed" };
    return renderToStaticMarkup(createElement(RoomProvider, { ports: createDemoPorts(), options: { storage: null } },
      createElement(IngestStatus, { activity })));
  };
  const markup = draw(conversations(12, 40));
  assert.match(markup, /<h3 id="[^"]+">Importing your newest conversations first<span class="ah-ingest-status__count"> · 12 of 40 conversations readable<\/span><\/h3>/);
  const live = [...markup.matchAll(/aria-live="polite" aria-atomic="true">([^<]*)</g)].map((m) => m[1]);
  assert.deepEqual(live, ["Importing your newest conversations first", ""]);
  assert.match(markup, /role="progressbar" aria-label="Importing your newest conversations first · 12 of 40 conversations readable"/);
  assert.match(markup, /data-ready-count="12" data-ready-total="40"/);
  // An interrupted feed says so, and claims no count it cannot refresh.
  assert.match(draw(conversations(12, 40), { connectionError: "gone" }), /<h3 id="[^"]+">Progress updates paused<\/h3>/);
});

test("past reading the card never says conversations are unavailable, and promises only what each close opens", () => {
  const closing = (counts, overrides = {}) => activityPhase(feed({ runtimes: [runtime({ state: "applying" })] }, { conversations: counts, ...overrides }));
  const opening = closing(conversations(700, 1_139));
  assert.equal(opening.key, "organizing");
  assert.equal(phaseHeading(opening), "Opening conversations as each agent finishes · 700 of 1,139 conversations readable");
  assert.doesNotMatch(opening.detail, /before they become available/);
  const all = closing(conversations(1_139, 1_139));
  assert.equal(phaseHeading(all), "All 1,139 conversations readable");
  assert.equal(all.spoken, "All conversations readable");
  assert.equal(all.detail, "Finishing the archive so search and charts can start.");
  const finalizing = (counts) => activityPhase(feed({ phase: "finalizing", runtimes: [runtime({ state: "done" })] }, { conversations: counts }));
  assert.equal(phaseHeading(finalizing(conversations(1_139, 1_139))), "All 1,139 conversations readable");
  assert.equal(phaseHeading(finalizing(conversations(1_100, 1_139))), "Finishing your archive · 1,100 of 1,139 conversations readable");
  // Publishing names the save and no count.
  assert.equal(phaseHeading(activityPhase(feed({ phase: "publishing", runtimes: [runtime({ state: "done" })] }, { conversations: conversations(9, 9) }))),
    "Saving your archive");
});

test("finishing says conversations may pause while the archive is saved", () => {
  for (const phase of ["finalizing", "publishing"]) {
    const view = activityPhase(feed({ phase, runtimes: [runtime({ state: "done" })] }));
    assert.match(view.detail, /Conversations may pause for a few seconds while the archive is saved\.$/);
    assert.equal(view.count ?? null, null, "no catalog, no count");
  }
});

test("a resumed build counts what the catalog says is readable, never its board, which starts at zero", () => {
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
  assert.equal(resumed.count ?? null, null);
  // The count is the catalog's, never the board's: nothing across a launch,
  // and what stayed readable after a pause in the same process.
  assert.equal(phaseHeading(activityPhase(feed({ projects: [project(9)] }, { resumed: true, conversations: conversations(0, 40) }))),
    "Finishing the import · 0 of 40 conversations readable");
  assert.equal(phaseHeading(activityPhase(feed({ projects: [project(0)] }, { resumed: true, conversations: conversations(12, 40) }))),
    "Finishing the import · 12 of 40 conversations readable");

  // It ends with the import, and a new daemon start that never restored is fresh.
  assert.equal(resumedRun(key, feed({ phase: "live" })), null);
  assert.equal(resumedRun(key, feed({}, { startedAt: 2 })), null);
  assert.equal(resumedRun(null, catchingUp), null);

  // A daemon that names its course says so itself: a room that opened after
  // the restoring phase still knows, and a fresh build is never resumed.
  const courseOf = (admission_course, phase = "backfilling") => feed({ phase, runtimes: [runtime({ admission_course })] });
  assert.equal(resumedRun(null, courseOf("at_publication")), "1:run");
  assert.equal(resumedRun(key, courseOf("per_tree")), null);
  assert.equal(resumedRun(null, courseOf("per_tree", "restoring")), null);
  assert.equal(resumedRun(null, courseOf("at_publication", "live")), null);
});

function renderProject(backfill, counts, ingestActive = true) {
  const row = { id: "p", name: "Project", root_paths: ["/work"], git_repo: null };
  const view = buildProjectViewModelFromSessions(row, [session("a", pending(1))], undefined, () => "claude-code", NOW, () => "#888");
  return renderToStaticMarkup(createElement(ProjectListItem, {
    project: view, maximumLifetimeCost: 0, isSelected: false, homeDirectory: null, currentTimestamp: NOW,
    ingestActive, backfill, conversations: counts, onProjectSelect: () => {},
  }));
}

test("the project card counts its readable conversations, the unit the lists draw", () => {
  const card = (backfill, readable, total, active) =>
    renderProject(backfill, { readable, total }, active).match(/ah-project-list-item__progress"[^>]*>([^<]*)</)?.[1] ?? null;
  assert.equal(card(project(3, 10), 3, 7), "3 of 7 conversations readable");
  assert.equal(card(project(1_234, 2_000), 1_234, 1_500), "1,234 of 1,500 conversations readable");
  // Nothing readable: not reached while none of its bytes are read, else the count.
  assert.equal(card({ ...project(0, 10), bytes_streamed: 0 }, 0, 7), "Not reached yet");
  assert.equal(card(null, 0, 7), "Not reached yet");
  assert.equal(card(project(0, 10), 0, 1), "0 of 1 conversation readable");
  assert.equal(card(project(10, 10), 7, 7), "All 7 readable · figures after the import");
  assert.equal(card(project(1, 1), 1, 1), "Readable · figures after the import");
  // No conversation counted yet, and the figures after the import.
  assert.equal(renderProject(project(0, 10), null).match(/ah-project-list-item__progress"[^>]*>([^<]*)</)?.[1], "Waiting for conversations");
  assert.equal(card(project(10, 10), 7, 7, false), "Preparing activity summaries");
  assert.doesNotMatch(renderProject(project(0, 10), { readable: 0, total: 7 }), /Reading conversations/);
});

test("once the archive is published the card moves on to search, though the phase still says publishing", () => {
  const publishing = (canonicalReady) => activityPhase(feed({ phase: "publishing", runtimes: [runtime({ state: "done" })] },
    { canonicalReady, conversations: conversations(9, 9) }));
  assert.equal(publishing(false).title, "Saving your archive");
  assert.deepEqual([publishing(true).key, publishing(true).title], ["search_waiting", "Preparing search"]);
  const done = activityPhase(feed({ phase: "live", runtimes: [runtime({ state: "done" })] }, { canonicalReady: true, searchReadiness: "ready" }));
  assert.deepEqual([done.key, done.title], ["ready", "Search ready"]);
});
