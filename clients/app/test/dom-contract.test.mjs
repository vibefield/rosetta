// The DOM readiness contract: the inert attributes an injected probe and a
// scripted reader use to tell what the room shows, so neither ever scrapes
// copy. Every value is derived from props or state the component already
// has (no state, effect, context or memo of its own); each is a short enum,
// a number or an id the room already holds. Copy may change; these may not
// without changing this list.
//
// Faces before the columns
//   .ah-empty[data-face]                 HistoryRoom, ColdStartPanel
//     connecting     reaching the index (the catalog has not answered)
//     not-answering  the catalog subscription closed
//     consent        awaiting consent, and the host gave no consent screen
//     cold-start     the first run before its first metadata batch; holds
//                    .ah-ingest-status
//   .ah-cols                             the columns are up (data-rail on|off)
//   [data-roll-call]                     the app's consent screen (RollCall)
//     looking   the census has not answered; no rows yet
//     ready     rows drawn; Begin is enabled when one is checked
//     starting  Begin pressed; waiting for the import to start
//   input[type=checkbox][data-agent-kind="<kind>"]   one per installed agent;
//                    read .checked and .disabled from the element
//   button[data-action="begin-ingest"]   Begin; .disabled with none checked
//                    or while starting
//
// The import card                        IngestStatus (in .ah-cold-start, and
//                    above the project list; gone once the panel closes)
//   .ah-ingest-status[data-state]
//     disconnected  the status feed is interrupted
//     complete      the archive and search are ready (briefly, then gone)
//     otherwise the step: discovering, metadata, restoring, reading,
//     organizing, finalizing, publishing, paused, pausing, failed,
//     search_failed, search_waiting, search_indexing, search_optimizing,
//     search_validating, search_publishing
//   .ah-ingest-status[data-ready-count]  integer: the conversations ready
//                    (Σ ProjectBackfill.sessions_done), the N of "· N ready".
//                    Always present; the words show it only when data-state
//                    is "reading", N > 0 and the run is not resumed
//
// Projects                               ProjectListItem
//   .ah-project-list-item[data-motion-key]  the project id (the unassigned
//                    bucket is "__rosetta_unassigned__")
//   .ah-project-list-item[data-cooking]  present while its figures wait
//                    (import running, or its stats not ready)
//   .ah-project-list-item[data-fill]     present while a byte meter fills it
//   .ah-sortseg button[disabled]         the token sort, while the import's
//                    order holds (until publication); its title says why
//
// Sessions                               SessionList, SessionListItem
//   .ah-session-list-item[data-motion-key]  the session id
//   .ah-session-list-item[data-standing]
//     not-imported  the import runs and has not reached it (stream_state
//                   pending); drawn in the "Not imported yet" group
//     unavailable   a metadata-only parent: no import will reach it
//     ready         neither: it reads as it always did
//   .ah-session-list-item[data-stream-state]  pending | streaming | indexed
//   .ah-session-list-item[data-cooking]  present while its figure slots
//                    shimmer (not indexed, no figures landed); never on a
//                    not-imported row, which has nothing read to cook
//   .ah-session-list-item[data-role]     main, or the session's role
//   .ah-day[data-not-imported]           the "Not imported yet" group's head;
//                    its rows follow it. A held list keeps its rows there
//                    after they flip: data-standing says where each one is now
//   .ah-list-updates[data-visible]       a held list has a new order waiting
//                    (one per list; pressing it applies the order)
//
// Selection (both lists)
//   [aria-current="true"]  on the selected row, or on its covering
//                    .ah-*-list-item__hit button when the host gave the row
//                    an act (the row then carries [data-host-act]); the row
//                    has .is-selected either way
//
// Summaries                              Veil, in the summary panels
//   .ah-veil         aggregate figures are not ready yet (role=status)
//
// The transcript                         TranscriptColumn and its parts
//   .tr-stage[data-session-key]  the open session's id; absent with none open
//   .tr-stage[data-ready]        "true" once the opening is revealed; absent
//                    while .tr-loading covers it
//   .tr-loading[data-loading-label]  which wait it shows:
//     error | team | seeking | preparing | loading   (.tr-loading[data-error]
//                    also marks the error)
//   .tr-sitting[data-sitting-key]  one per sitting drawn; the timeline id of
//                    its first row
//   .ah-empty[data-import-face]
//     not-imported  "Not imported yet"; also kept between a tree's flip and
//                   its resync, while the row already reads otherwise
//     unavailable   "Transcript not available" (a metadata-only parent)
//   .ah-empty[data-import-course]  newest-first | resumed | stopped: the
//                    course the not-imported face's promise is chosen from
//   .tr-mark[data-import-banner]   an open transcript during the import
//   .tr-mark[data-transcript-unavailable]  a metadata-only parent, above the
//                    conversations it started
//
// Search                                 SearchSheet
//   .ah-sheet__row[data-hit-kind]  title (a title match, also [data-title])
//                    | fulltext (a full-text hit); [aria-selected] is the
//                    cursor. Recent queries are .ah-sheet__recent rows.
//
// The not-answering and consent faces, .ah-cols[data-rail], and data-ready
// on a stage with a session, need live ports or layout: room-host.electron.mjs
// and transcript-loading.electron.mjs cover them on glass. Every other entry
// is asserted below. Nothing here reads a day head, so the clock's zone does
// not matter.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import ts from "typescript";
import { HistoryRoom } from "../../room/src/HistoryRoom.tsx";
import { ColdStartPanel } from "../../room/src/components/ColdStart.tsx";
import { IngestStatus } from "../../room/src/components/IngestStatus.tsx";
import { ListActivityNotice } from "../../room/src/components/ListActivityNotice.tsx";
import { TranscriptColumn } from "../../room/src/components/Transcript.tsx";
import { ProjectListItem } from "../../room/src/components/projects/ProjectListItem.tsx";
import { ProjectsColumn } from "../../room/src/components/projects/ProjectsColumn.tsx";
import { OptionRow, SheetTitles } from "../../room/src/components/search/SearchSheet.tsx";
import { SessionList } from "../../room/src/components/sessions/SessionList.tsx";
import { SessionListItem } from "../../room/src/components/sessions/SessionListItem.tsx";
import { TranscriptDocument } from "../../room/src/components/transcript/Document.tsx";
import { ImportEmptyState } from "../../room/src/components/transcript/ImportState.tsx";
import { Veil } from "../../room/src/components/ui/Veil.tsx";
import { TranscriptLoading } from "../../room/src/components/transcript/TranscriptLoading.tsx";
import { transcriptModel } from "../../room/src/components/transcript/document-model.ts";
import { buildThreadIndex } from "../../room/src/components/transcript/thread-index.ts";
import { createDemoPorts } from "../../room/src/fixtures/index.ts";
import { HostInputsProvider } from "../../room/src/host-inputs.ts";
import { AgentMark, lastActive } from "../../room/src/index.ts";
import { activityPhase } from "../../room/src/lib/ingest-activity.ts";
import { buildProjectViewModelFromSessions, UNASSIGNED_PROJECT_ID } from "../../room/src/lib/model.ts";
import { matchSessionTitles } from "../../room/src/lib/title-search.ts";
import { RoomProvider } from "../../room/src/room-context.tsx";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const React = fromRoom("react");
const { createElement } = React;
const { renderToStaticMarkup } = fromRoom("react-dom/server");

const NOW = Date.UTC(2026, 9, 2, 12);
const HOUR = 3_600_000;
const nothing = () => undefined;

/** Every start tag in a markup, with its attributes (a bare one reads ""). */
function elements(markup) {
  return [...markup.matchAll(/<([a-z][\w-]*)((?:\s+[^\s=>/]+(?:="[^"]*")?)*)\s*\/?>/g)].map(([, tag, rest]) => ({
    tag,
    attrs: Object.fromEntries([...rest.matchAll(/\s+([^\s=>/]+)(?:="([^"]*)")?/g)]
      .map(([, name, value = ""]) => [name, value])),
  }));
}
const classed = (markup, name) =>
  elements(markup).filter((element) => (element.attrs.class ?? "").split(" ").includes(name));
function only(list, what) {
  assert.equal(list.length, 1, `exactly one ${what}`);
  return list[0].attrs;
}

function session(id, overrides = {}) {
  return {
    id, native_session_id: `${id}-native`, runtime_id: "rt", project_id: "p", cwd: "/work",
    parent_session_id: null, delegation: null, root_session_id: id, role: "main",
    title: `Title ${id}`, summary: null, started_at: NOW - 2 * HOUR, updated_at: NOW - HOUR,
    activity: { first_at: NOW - 2 * HOUR, last_at: NOW - HOUR }, stream_state: "indexed", ...overrides,
  };
}
const pending = (overrides = {}) => ({ stream_state: "pending", activity: null, ...overrides });
// What the metadata pass makes for a parent whose own transcript was not found.
const metadataOnly = (overrides = {}) => ({
  stream_state: "pending", activity: null, cwd: null, title: null, summary: null, delegation: null,
  project_id: null, ...overrides,
});

/* ── faces before the columns ─────────────────────────────────────────── */

test("the face the room draws while it reaches the index says so", () => {
  const markup = renderToStaticMarkup(createElement(HistoryRoom, { ...createDemoPorts(), storage: null }));
  assert.equal(only(elements(markup).filter((element) => "data-face" in element.attrs), "face")["data-face"], "connecting");
  assert.equal(classed(markup, "ah-cols").length, 0, "no columns behind it");
});

const runtime = (overrides = {}) => ({ kind: "claude-code", runtime_id: "rt", label: null, state: "streaming",
  files_total: 100, files_skimmed: 100, sessions_total: 80, bytes_total: 1_000_000, bytes_streamed: 400_000,
  records: 300, ...overrides });
const board = (done, projectId = "p") => ({ project_id: projectId, sessions_total: 40, sessions_done: done,
  bytes_total: 1_000_000, bytes_streamed: 400_000 });
const run = { id: "run", index_id: "ix", revision: 1, state: "running", phase: "capturing", created_at: 0,
  updated_at: 0, progress_epoch: "run:1", runtime_ids: ["rt"], allowed_actions: [], error: null };
function activityOf(ingest = {}, overrides = {}) {
  const feed = {
    ingest: { phase: "backfilling", projects: [board(0)], runtimes: [runtime()], run, ...ingest },
    canonicalReady: false, searchReadiness: "building", searchProgress: null, connectionError: null,
    startedAt: 1, uptimeMs: 5_000, receivedAt: 1_000, ...overrides,
  };
  return { phase: activityPhase(feed), feed, footer: "Connected", elapsed: "5s elapsed" };
}
const inRoom = (element) => renderToStaticMarkup(
  createElement(RoomProvider, { ports: createDemoPorts(), options: { storage: null } }, element));

test("the cold start is a face of its own, holding the import card", () => {
  const markup = inRoom(createElement(ColdStartPanel, { activity: activityOf({ projects: [board(3)] }) }));
  const face = only(classed(markup, "ah-cold-start"), "cold start");
  assert.equal(face["data-face"], "cold-start");
  const card = only(classed(markup, "ah-ingest-status"), "import card");
  assert.equal(card["data-state"], "reading");
  assert.equal(card["data-ready-count"], "3");
});

/* ── the import card ──────────────────────────────────────────────────── */

test("the import card carries the ready count, also where its words leave it out", () => {
  const card = (ingest, overrides) =>
    only(classed(inRoom(createElement(IngestStatus, { activity: activityOf(ingest, overrides) })), "ah-ingest-status"), "card");
  // Drawn: "· 12 ready", over two projects and the unassigned bucket.
  const twelve = card({ projects: [board(5), board(7, null)] });
  assert.deepEqual([twelve["data-state"], twelve["data-ready-count"]], ["reading", "12"]);
  assert.deepEqual([card({ projects: [board(0)] })["data-state"], card({ projects: [board(0)] })["data-ready-count"]],
    ["reading", "0"]);
  // Left out of the words, still carried: an interrupted feed, a resumed
  // build, and the steps that draw no count.
  const cut = card({ projects: [board(12)] }, { connectionError: "gone" });
  assert.deepEqual([cut["data-state"], cut["data-ready-count"]], ["disconnected", "12"]);
  const resumed = card({ projects: [board(9)] }, { resumed: true });
  assert.deepEqual([resumed["data-state"], resumed["data-ready-count"]], ["reading", "9"]);
  const saving = card({ phase: "publishing", projects: [board(40)], runtimes: [runtime({ state: "done" })] });
  assert.deepEqual([saving["data-state"], saving["data-ready-count"]], ["publishing", "40"]);
  const paused = card({ projects: [board(4)], run: { ...run, state: "paused" } });
  assert.deepEqual([paused["data-state"], paused["data-ready-count"]], ["paused", "4"]);
  const done = card({ phase: "live", projects: [board(40)], runtimes: [runtime({ state: "done" })] },
    { canonicalReady: true, searchReadiness: "ready" });
  assert.deepEqual([done["data-state"], done["data-ready-count"]], ["complete", "40"]);
  // A daemon too old to carry the board counts nothing.
  assert.equal(card({ projects: undefined })["data-ready-count"], "0");
});

/* ── the project list ─────────────────────────────────────────────────── */

function projectRow({ id = "p", ingestActive = true, isSelected = false, act = null } = {}) {
  const row = { id, name: "Project", root_paths: ["/work"], git_repo: null };
  const view = buildProjectViewModelFromSessions(row, [session("a", { project_id: id })], undefined,
    () => "claude-code", NOW, () => "#888");
  const item = createElement(ProjectListItem, {
    project: view, maximumLifetimeCost: 0, isSelected, homeDirectory: null, currentTimestamp: NOW,
    ingestActive, runtimeStates: ["streaming"], backfill: board(3, id), onProjectSelect: nothing,
  });
  return renderToStaticMarkup(act === null ? item
    : createElement(HostInputsProvider, { value: { projectRow: () => ({ act }) } }, item));
}
const act = { key: "pin", label: "Pin", content: "Pin", onPress: nothing };

test("a project row is keyed by its project id, and says while its figures wait", () => {
  const cooking = only(classed(projectRow(), "ah-project-list-item"), "project row");
  assert.equal(cooking["data-motion-key"], "p");
  assert.equal(cooking["data-cooking"], "");
  assert.equal(cooking["aria-current"], undefined);
  const settled = only(classed(projectRow({ ingestActive: false }), "ah-project-list-item"), "project row");
  // Its stats are not hydrated here, so it still waits on them.
  assert.equal(settled["data-cooking"], "");
  assert.equal(only(classed(projectRow({ id: UNASSIGNED_PROJECT_ID }), "ah-project-list-item"), "row")["data-motion-key"],
    "__rosetta_unassigned__");
  // Selected: on the row, or on the covering button when the host gave it an act.
  assert.equal(only(classed(projectRow({ isSelected: true }), "ah-project-list-item"), "row")["aria-current"], "true");
  const boxed = projectRow({ isSelected: true, act });
  const box = only(classed(boxed, "ah-project-list-item"), "row");
  assert.equal(box["data-host-act"], "");
  assert.equal(box["aria-current"], undefined);
  assert.equal(only(classed(boxed, "ah-project-list-item__hit"), "hit")["aria-current"], "true");
});

test("the token sort waits while the import's order holds, and says why", () => {
  const sortButtons = (bootstrapping) => elements(inRoom(createElement(ProjectsColumn, {
    projects: [], activityOrder: { sessions: new Map(), projects: new Map(), bootstrapping },
    selectedProjectId: null, sort: "token", onSortChange: nothing, homeDirectory: null, currentTimestamp: NOW,
    ingest: activityOf().feed.ingest, activity: activityOf(), onProjectSelect: nothing,
  }))).filter((element) => element.tag === "button" && "aria-pressed" in element.attrs);
  const importing = sortButtons(true);
  const token = importing.find((button) => button.attrs["aria-label"]?.startsWith("token"));
  assert.equal("disabled" in token.attrs, true);
  assert.equal(token.attrs.title, "Spend ranks arrive after the import");
  // A token choice reads as recent until then.
  assert.equal(importing.find((button) => button.attrs["aria-pressed"] === "true")?.attrs["aria-label"], undefined);
  const live = sortButtons(false);
  assert.equal(live.some((button) => "disabled" in button.attrs), false);
  assert.equal(live.filter((button) => button.attrs["aria-pressed"] === "true").length, 1);
});

/* ── the session list ─────────────────────────────────────────────────── */

function renderList(sessions, { duringBackfill = true, parents = new Set(), selected = null, hostAct = null } = {}) {
  const list = createElement(SessionList, {
    sessions, factsBySessionId: new Map(), getRuntimeKind: () => "claude-code",
    selectedSessionId: selected, currentTimestamp: NOW, duringBackfill, pending: false,
    onSessionSelect: nothing, parentIds: parents,
  });
  return renderToStaticMarkup(hostAct === null ? list
    : createElement(HostInputsProvider, { value: { sessionRow: () => ({ act: hostAct }) } }, list));
}
const rowsOf = (markup) => new Map(classed(markup, "ah-session-list-item")
  .map(({ attrs }) => [attrs["data-motion-key"], attrs]));

test("a session row says where it stands with the import, from the stream state it reads", () => {
  const rows = [
    session("open"),
    session("half", { stream_state: "streaming" }),
    session("waiting", pending()),
    session("orphan", metadataOnly()),
  ];
  const parents = new Set(["orphan"]);
  const during = renderList(rows, { parents, selected: "open" });
  const drawn = rowsOf(during);
  assert.deepEqual([...drawn.keys()].sort(), ["half", "open", "orphan", "waiting"], "keyed by the session id");
  const facts = (id) => {
    const row = drawn.get(id);
    return [row["data-standing"], row["data-stream-state"], "data-cooking" in row];
  };
  assert.deepEqual(facts("open"), ["ready", "indexed", false]);
  assert.deepEqual(facts("half"), ["ready", "streaming", true]);
  assert.deepEqual(facts("waiting"), ["not-imported", "pending", false]);
  assert.deepEqual(facts("orphan"), ["unavailable", "pending", false]);
  assert.equal(drawn.get("open")["aria-current"], "true");
  assert.equal([...drawn.values()].filter((row) => "aria-current" in row).length, 1);
  // The not-imported group's head, and the waiting row after it.
  const heads = classed(during, "ah-day");
  assert.equal(heads.filter(({ attrs }) => "data-not-imported" in attrs).length, 1);
  assert.match(during, /<div class="ah-day" data-not-imported="">[^<]*<\/div><button[^>]*data-motion-key="waiting"/);

  // Once the import is over a Pending row reads as it always did; a
  // metadata-only parent is still unavailable.
  const after = rowsOf(renderList(rows, { duringBackfill: false, parents }));
  assert.deepEqual([after.get("waiting")["data-standing"], after.get("waiting")["data-stream-state"]], ["ready", "pending"]);
  assert.equal(after.get("orphan")["data-standing"], "unavailable");
  // Without children the same row is a real session the import has not reached.
  assert.equal(rowsOf(renderList(rows, { parents: new Set() })).get("orphan")["data-standing"], "not-imported");
});

test("a session row with a host act keeps its facts on the row and its selection on the covering button", () => {
  const markup = renderToStaticMarkup(createElement(HostInputsProvider, { value: { sessionRow: () => ({ act }) } },
    createElement(SessionListItem, {
      session: session("w", pending()), facts: null, runtimeKind: "claude-code", isSelected: true,
      currentTimestamp: NOW, duringBackfill: true, onSessionSelect: nothing,
    })));
  const row = only(classed(markup, "ah-session-list-item"), "row");
  assert.equal(row["data-motion-key"], "w");
  assert.equal(row["data-host-act"], "");
  assert.equal(row["data-standing"], "not-imported");
  assert.equal(row["data-stream-state"], "pending");
  assert.equal(row["aria-current"], undefined);
  assert.equal(only(classed(markup, "ah-session-list-item__hit"), "hit")["aria-current"], "true");

  // Every standing and stream state reads the same on the host-act row as
  // on the plain one, during the import and after it.
  const rows = [
    session("open"),
    session("half", { stream_state: "streaming" }),
    session("waiting", pending()),
    session("orphan", metadataOnly()),
  ];
  const parents = new Set(["orphan"]);
  const facts = (attrs) => [attrs["data-standing"], attrs["data-stream-state"], "data-cooking" in attrs, attrs["data-role"]];
  const expected = {
    true: { open: ["ready", "indexed", false, "main"], half: ["ready", "streaming", true, "main"],
      waiting: ["not-imported", "pending", false, "main"], orphan: ["unavailable", "pending", false, "main"] },
    false: { waiting: ["ready", "pending"], orphan: ["unavailable", "pending"] },
  };
  for (const duringBackfill of [true, false]) {
    const plain = rowsOf(renderList(rows, { duringBackfill, parents }));
    const boxed = rowsOf(renderList(rows, { duringBackfill, parents, hostAct: act }));
    assert.deepEqual([...boxed.keys()].sort(), ["half", "open", "orphan", "waiting"]);
    for (const [id, attrs] of boxed) {
      assert.equal(attrs["data-host-act"], "", `${id} has the host act`);
      assert.deepEqual(facts(attrs), facts(plain.get(id)), `${id} reads alike with and without the act`);
      const want = expected[duringBackfill][id];
      if (want) assert.deepEqual(facts(attrs).slice(0, want.length), want, `${id} during=${duringBackfill}`);
    }
  }
});

/* ── existing hooks the probe relies on ───────────────────────────────── */

test("a held list's update pill, the summary veil and a filling project row mark themselves", () => {
  const pill = (pending) => only(elements(renderToStaticMarkup(createElement(ListActivityNotice, {
    pending, onShow: nothing, noun: "sessions",
  }))).filter((element) => "data-list-updates" in element.attrs), "update pill");
  const waiting = pill(true);
  assert.deepEqual([waiting["data-visible"], "hidden" in waiting], ["", false]);
  const idle = pill(false);
  assert.deepEqual([idle["data-visible"], "hidden" in idle], [undefined, true]);

  const veil = only(classed(renderToStaticMarkup(createElement(Veil, { label: "preparing", progress: null })), "ah-veil"),
    "veil");
  assert.equal(veil.role, "status");

  // A running import with a board: the row's byte meter fills it.
  assert.equal(only(classed(projectRow(), "ah-project-list-item"), "project row")["data-fill"], "");
  assert.equal(only(classed(projectRow({ act }), "ah-project-list-item"), "project row")["data-fill"], "");
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
  return {
    rows, snapshotReady, error: null, retry: nothing, timelineMeta: null, status: "live",
    watermark: null, earlier: { kind: "none" }, earlierNotice: null, earlierAbove: null,
    landed: null, holdEarlier: nothing, revealEarlier: nothing, retryEarlier: nothing,
    dismissEarlierNotice: nothing, canLoadEarlier: false, loadEarlier: nothing,
    loadAnchor: async () => false,
  };
}

function renderColumn(row, { duringBackfill = false, snapshotReady = false } = {}) {
  return inRoom(createElement(TranscriptColumn, {
    session: row, duringBackfill, sessions: row === null ? [] : [row], project: null,
    feed: feedOf([], snapshotReady), focus: null, onFocusConsumed: nothing, onReceipt: nothing,
    emptyPrompt: "Pick a session", stepper: null, onStepMatch: nothing, onDismissStepper: nothing,
    getRuntimeKind: () => "claude-code", homeDirectory: null,
  }));
}

test("the stage names the open session, and is ready only once nothing covers it", () => {
  // Opening: the loader covers the stage, which names its session already.
  const opening = renderColumn(session("s1"));
  const stage = only(classed(opening, "tr-stage"), "stage");
  assert.equal(stage["data-session-key"], "s1");
  assert.equal(stage["data-ready"], undefined);
  assert.equal(only(classed(opening, "tr-loading"), "loader")["data-loading-label"], "loading");
  // During the import, an unindexed session's loader says it is preparing.
  const preparing = renderColumn(session("s2", { stream_state: "streaming" }), { duringBackfill: true });
  assert.equal(only(classed(preparing, "tr-stage"), "stage")["data-session-key"], "s2");
  assert.equal(only(classed(preparing, "tr-loading"), "loader")["data-loading-label"], "preparing");
  // No session: nothing to open, nothing named, ready.
  const none = only(classed(renderColumn(null), "tr-stage"), "stage");
  assert.deepEqual([none["data-session-key"], none["data-ready"]], [undefined, "true"]);
});

test("the loader names which wait it shows", () => {
  const label = (props) => only(classed(renderToStaticMarkup(createElement(TranscriptLoading, {
    error: null, waitingForTeam: false, seeking: false, canContinue: false, indexing: false,
    onRetry: nothing, onContinue: nothing, ...props,
  })), "tr-loading"), "loader");
  assert.equal(label({})["data-loading-label"], "loading");
  assert.equal(label({ indexing: true })["data-loading-label"], "preparing");
  assert.equal(label({ seeking: true, indexing: true })["data-loading-label"], "seeking");
  assert.equal(label({ waitingForTeam: true, seeking: true })["data-loading-label"], "team");
  const failed = label({ error: "gone", waitingForTeam: true });
  assert.deepEqual([failed["data-loading-label"], failed["data-error"]], ["error", "true"]);
  assert.equal(label({})["data-error"], undefined);
});

function renderDocument(row, { rows = [], duringBackfill = true, importCourse } = {}) {
  return inRoom(createElement(TranscriptDocument, {
    session: row, model: transcriptModel(rows, row, null, NOW), feed: feedOf(rows), duringBackfill, importCourse,
    index: buildThreadIndex([row]), focus: null, highlightedTimelineId: null,
    onFocusConsumed: nothing, onReceipt: nothing, onOpenAttachment: nothing, onOpenPath: nothing,
    getRuntimeKind: () => "claude-code", homeDirectory: null, onPillPress: null,
  }));
}
const importFace = (markup) =>
  elements(markup).filter((element) => "data-import-face" in element.attrs);

test("the not-imported face names itself and the course its promise comes from", () => {
  const waiting = session("w", pending());
  for (const [course, expected] of [[undefined, "newest-first"], ["newest-first", "newest-first"],
    ["resumed", "resumed"], ["stopped", "stopped"]]) {
    const face = only(importFace(renderDocument(waiting, { importCourse: course })), "import face");
    assert.deepEqual([face.class, face["data-import-face"], face["data-import-course"]],
      ["ah-empty", "not-imported", expected]);
  }
  // A metadata-only parent's empty face: unavailable, and no course to keep.
  const unavailable = only(importFace(renderToStaticMarkup(createElement(ImportEmptyState, {
    standing: "unavailable", duringBackfill: true, course: "resumed",
  }))), "import face");
  assert.deepEqual([unavailable["data-import-face"], unavailable["data-import-course"]], ["unavailable", undefined]);
  // An open session reads under the banner, in sittings, and has no face.
  const rows = [prompt("o", "hello"), prompt("o", "again")];
  const open = renderDocument(session("o"), { rows });
  assert.equal(importFace(open).length, 0);
  assert.equal(elements(open).filter((element) => "data-import-banner" in element.attrs).length, 1);
  const sittings = classed(open, "tr-sitting");
  assert.ok(sittings.length >= 1);
  assert.equal(sittings[0].attrs["data-sitting-key"], rows[0].timeline_id);
});

/* ── the search sheet ─────────────────────────────────────────────────── */

test("a search row says whether it is a title match or a full-text hit", () => {
  const titled = session("s1", { title: "Fix the import meter" });
  const project = { row: { id: "p", name: "rosetta", root_paths: [], git_repo: null }, tint: "#888" };
  const titles = matchSessionTitles("import", [titled], new Map([["p", project]]),
    { project: null, agent: null, runtimeKind: () => "claude-code" });
  const titleRow = only(classed(renderToStaticMarkup(createElement(SheetTitles, {
    titles, query: "import", active: 0, nowMs: NOW, getRuntimeKind: () => "claude-code",
    onHover: nothing, onOpen: nothing,
  })), "ah-sheet__row"), "title row");
  assert.deepEqual([titleRow["data-hit-kind"], titleRow["data-title"], titleRow["aria-selected"]], ["title", "", "true"]);

  const hit = { timeline_id: "t1", session_id: "s1", kind: "user", at: NOW - HOUR, snippet: "the [import] meter", score: -1 };
  const hitRow = only(classed(renderToStaticMarkup(createElement(OptionRow, {
    option: { type: "option", key: "s1:t1", id: "ah-sheet-hit-0", index: 0, project, session: titled, hit },
    active: false, home: null, nowMs: NOW, runtimeKind: "claude-code", onHover: nothing, onOpen: nothing,
  })), "ah-sheet__row"), "hit row");
  assert.deepEqual([hitRow["data-hit-kind"], hitRow["data-title"], hitRow["aria-selected"]], ["fulltext", undefined, "false"]);
});

/* ── the app's roll call ──────────────────────────────────────────────── */

// The roll call's rows arrive through an effect, which a static render never
// runs. Its own module runs here with its four `useState` slots seeded (rows,
// checked, submitting, error, in that order) and React's other hooks as
// they are, so the markup is the component's own for each state.
const ROLL_CALL = new URL("../renderer/src/components/RollCall.tsx", import.meta.url);
function rollCall({ rows = null, checked = new Set(), submitting = false } = {}) {
  const seeds = [rows, checked, submitting, null];
  const initials = [];
  const react = {
    ...React,
    useState(initial) {
      initials.push(initial);
      return [seeds[initials.length - 1], nothing];
    },
  };
  const modules = {
    react,
    "react/jsx-runtime": fromRoom("react/jsx-runtime"),
    "@vibefield/rosetta-room": { AgentMark, lastActive },
  };
  const source = readFileSync(ROLL_CALL, "utf8");
  const { outputText } = ts.transpileModule(source, {
    fileName: fileURLToPath(ROLL_CALL),
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  });
  const module = { exports: {} };
  new Function("require", "module", "exports", outputText)((name) => {
    assert.ok(name in modules, `RollCall imports ${name}`);
    return modules[name];
  }, module, module.exports);
  const client = { discoveries: () => new Promise(nothing), enableRuntimes: async () => undefined };
  const markup = renderToStaticMarkup(createElement(module.exports.RollCallPanel, { client }));
  assert.deepEqual(initials.map((value) => value instanceof Set ? "set" : String(value)), ["null", "set", "false", "null"],
    "the seeds follow the panel's useState order");
  return markup;
}
const discovery = (kind, installed = true) => ({ kind, data_root: `~/.${kind}`, installed, files: 3, sessions: 3,
  bytes: 9_000, newest_modified_ms: NOW - HOUR, enabled: false });

test("the roll call names its state, each agent's toggle and the start button", () => {
  const panel = (markup) => only(elements(markup).filter((element) => "data-roll-call" in element.attrs), "roll call");
  const looking = rollCall();
  assert.equal(panel(looking)["data-roll-call"], "looking");
  assert.equal(elements(looking).filter((element) => element.tag === "input" || element.tag === "button").length, 0);

  const rows = [discovery("claude-code"), discovery("codex"), discovery("gemini", false)];
  const ready = rollCall({ rows, checked: new Set(["claude-code"]) });
  assert.equal(panel(ready)["data-roll-call"], "ready");
  const toggles = elements(ready).filter((element) => "data-agent-kind" in element.attrs);
  assert.deepEqual(toggles.map(({ tag, attrs }) => [tag, attrs.type, attrs["data-agent-kind"], "checked" in attrs]),
    [["input", "checkbox", "claude-code", true], ["input", "checkbox", "codex", false]], "installed agents only");
  const begin = only(elements(ready).filter((element) => element.attrs["data-action"] === "begin-ingest"), "begin");
  assert.equal(begin.disabled, undefined);
  assert.equal(only(elements(rollCall({ rows })).filter((element) => element.attrs["data-action"] === "begin-ingest"),
    "begin").disabled, "", "nothing checked");

  const starting = rollCall({ rows, checked: new Set(["claude-code"]), submitting: true });
  assert.equal(panel(starting)["data-roll-call"], "starting");
  assert.equal(only(elements(starting).filter((element) => element.attrs["data-action"] === "begin-ingest"), "begin").disabled, "");
  assert.ok(elements(starting).filter((element) => "data-agent-kind" in element.attrs).every(({ attrs }) => "disabled" in attrs));
});
