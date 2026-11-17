// R10 — the host's mark for the room's waits (`RoomWaitMark`). With one, a
// wait that fills a place is a HOLD — the mark, one line under it, its count
// under that — the import card wears the mark at its head, its words kept to
// the step's title and its count and a fraction it knows drawn as a
// colourless hairline, and nothing sweeps beside the mark; a paused wait's
// mark stands at rest. With none, every wait is the room's own, as rosetta's
// app shows it (room-meters and room-not-answering hold those). The server
// render covers the faces; the hold's timing is `afterWait`'s.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { HistoryRoom } from "../../room/src/HistoryRoom.tsx";
import { coldStartHold } from "../../room/src/components/ColdStart.tsx";
import { IngestStatus } from "../../room/src/components/IngestStatus.tsx";
import { TranscriptLoading } from "../../room/src/components/transcript/TranscriptLoading.tsx";
import { Meter, Veil } from "../../room/src/components/ui/index.ts";
import { createDemoPorts } from "../../room/src/fixtures/index.ts";
import { HostInputsProvider } from "../../room/src/host-inputs.ts";
import { PAUSED_UPDATES, activityPhase } from "../../room/src/lib/ingest-activity.ts";
import { HOLD_FLOOR_MS, HOLD_LATE_MS, afterWait } from "../../room/src/lib/use-standing-hold.ts";
import { RoomProvider } from "../../room/src/room-context.tsx";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const { createElement } = fromRoom("react");
const { renderToStaticMarkup } = fromRoom("react-dom/server");

/** A host's mark that says where it was put and whether it rests. */
const waitMark = (extra = {}) => ({
  mark: (place, { resting }) => createElement("i", { "data-mark": place, "data-resting": String(resting) }),
  ...extra,
});
/** A part in the room, as HistoryRoom places it (the import card's controls read the room's ports). */
const inRoom = (element) => createElement(RoomProvider, { ports: createDemoPorts(), options: { storage: null } }, element);
const marked = (element) => createElement(HostInputsProvider, { value: { waitMark: waitMark() } }, inRoom(element));

const runtime = (overrides = {}) => ({ kind: "codex", state: "streaming", files_total: 100,
  files_skimmed: 100, sessions_total: 80, bytes_total: 1_000_000, bytes_streamed: 400_000,
  records: 300, ...overrides });
const feed = (overrides = {}) => ({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime()] },
  canonicalReady: false, searchReadiness: "building", searchProgress: null, connectionError: null,
  startedAt: 1, uptimeMs: 5_000, receivedAt: 1_000, ...overrides });
const activityOf = (overrides = {}) => {
  const given = feed(overrides);
  return { phase: activityPhase(given), feed: given, footer: "Connected", elapsed: "2m 10s" };
};
const loading = (overrides = {}) => ({
  error: null, waitingForTeam: false, seeking: false, canContinue: false, indexing: false,
  onRetry() {}, onContinue() {}, ...overrides,
});

test("with no mark the room keeps its own waits: the connecting face sweeps and the card says IMPORTING HISTORY", () => {
  const room = renderToStaticMarkup(createElement(HistoryRoom, { ...createDemoPorts(), storage: null }));
  assert.doesNotMatch(room, /ah-hold/);
  assert.match(room, /data-face="connecting"/);
  assert.match(room, /data-indeterminate=""/);
  const card = renderToStaticMarkup(inRoom(createElement(IngestStatus, { activity: activityOf() })));
  assert.match(card, /IMPORTING HISTORY/);
  assert.doesNotMatch(card, /data-marked/);
});

test("in the host's mark, reaching the index is a hold: the mark, the host's title, its words under it, no sweep", () => {
  const markup = renderToStaticMarkup(createElement(HistoryRoom, {
    ...createDemoPorts(),
    storage: null,
    connectingTitle: "Opening History…",
    unavailable: "The agent index is starting",
    waitMark: waitMark(),
  }));
  assert.match(markup, /<div class="ah-hold" data-place="view" data-face="connecting" role="status"/);
  assert.match(markup, /<i data-mark="view" data-resting="false"><\/i>/);
  assert.match(markup,
    /<p class="ah-hold__line">Opening History…<\/p><p class="ah-hold__sub">The agent index is starting<\/p>/);
  assert.doesNotMatch(markup, /data-indeterminate/, "the mark is the sign of working: nothing sweeps beside it");
  assert.doesNotMatch(markup, /class="ah-empty"/, "the hold stands in the face's place");
  assert.doesNotMatch(markup, /data-carried/, "late, as the veil is, unless the host's own hold stands there");
});

test("a host whose own hold already stands has the room's first hold carried: no late, no fade", () => {
  const markup = renderToStaticMarkup(createElement(HistoryRoom, {
    ...createDemoPorts(), storage: null, waitMark: waitMark({ carried: true }),
  }));
  assert.match(markup, /<div class="ah-hold" data-place="view" data-face="connecting" data-carried=""/);
});

test("a hold is late, stands its floor once it has shown, and leaves after it", () => {
  const asked = 10_000;
  const shownAt = asked + HOLD_LATE_MS;
  assert.equal(afterWait(asked + 50, shownAt), "gone", "a wait answered before the hold showed shows none");
  assert.equal(afterWait(shownAt, shownAt), "floor");
  assert.equal(afterWait(shownAt + HOLD_FLOOR_MS - 1, shownAt), "floor", "a hold that showed stands 800ms");
  assert.equal(afterWait(shownAt + HOLD_FLOOR_MS, shownAt), "leave");
});

test("the cold start holds the room's own step words, the ellipsis of work under way, its count under them", () => {
  assert.deepEqual(coldStartHold(null), { words: { key: "discovering", line: "Finding your history…" }, resting: false });
  const discovering = {
    phase: { key: "discovering", title: "Finding your history", detail: "", amount: "Looking for conversations…",
      step: 1, current: 0, total: 0, unit: null, fraction: null, active: true },
    feed: feed(), footer: "", elapsed: "",
  };
  assert.deepEqual(coldStartHold(discovering).words,
    { key: "discovering", line: "Finding your history…", sub: null },
    "the first step's title stands alone: its amount counts nothing and says the same wait again");
  const metadata = activityOf({ ingest: { phase: "backfilling", projects: [],
    runtimes: [runtime({ state: "skimming", files_skimmed: 1_333, files_total: 1_667 })] } });
  assert.deepEqual(coldStartHold(metadata),
    { words: { key: "metadata", line: "Discovering conversations…", sub: "1,333 files checked" }, resting: false });
  // motion means working: a paused import says its own title, at rest; silent updates the card's words, at rest
  const paused = activityOf({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime()],
    run: { state: "paused" } } });
  assert.deepEqual(coldStartHold(paused),
    { words: { key: "paused", line: "History import paused", sub: paused.phase.amount }, resting: true });
  const silent = activityOf({ connectionError: "connection refused" });
  assert.deepEqual(coldStartHold(silent),
    { words: { key: "paused-updates", line: PAUSED_UPDATES.title, sub: PAUSED_UPDATES.detail }, resting: true });
});

test("the import card wears the mark where IMPORTING HISTORY stood and keeps its words to the title and its count", () => {
  const reading = activityOf();
  const markup = renderToStaticMarkup(marked(createElement(IngestStatus, { activity: reading })));
  assert.match(markup, /<section class="ah-ingest-status" aria-labelledby="[^"]+" data-marked="" data-state="reading"/);
  assert.match(markup,
    /<span class="ah-ingest-status__mark"><i data-mark="card" data-resting="false"><\/i><\/span><span>Step 2 of 4<\/span>/);
  assert.doesNotMatch(markup, /IMPORTING HISTORY/);
  const head = markup.slice(0, markup.indexOf("<details"));
  assert.match(head, new RegExp(`<h3 id="[^"]+">${reading.phase.title}</h3><p class="ah-ingest-status__sub">${reading.phase.amount}</p>`));
  // a fraction the step knows is the hairline; the detail, the amount, the connection and the timer fold away
  assert.match(head, /role="progressbar"[^>]*aria-valuenow="40"/);
  assert.doesNotMatch(head, /ah-ingest-status__detail|ah-ingest-status__amount|ah-ingest-status__footer/);
  const folded = markup.slice(markup.indexOf("<details"));
  assert.match(folded, /<summary>Agent details/);
  for (const part of ["ah-ingest-status__detail", "ah-ingest-status__amount", "ah-ingest-status__footer"]) {
    assert.match(folded, new RegExp(part), `${part} under Agent details`);
  }
  assert.match(folded, /2m 10s/);
});

test("beside the mark a step that knows no fraction draws no bar, and silent updates stand the mark at rest", () => {
  const organizing = activityOf({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime({ state: "applying" })] } });
  assert.equal(organizing.phase.fraction, null);
  const quiet = renderToStaticMarkup(marked(createElement(IngestStatus, { activity: organizing })));
  assert.doesNotMatch(quiet.slice(0, quiet.indexOf("<details")), /role="progressbar"/);
  const silent = renderToStaticMarkup(marked(createElement(IngestStatus, { activity: activityOf({ connectionError: "x" }) })));
  assert.match(silent, /data-state="disconnected"/);
  assert.match(silent, /<i data-mark="card" data-resting="true"><\/i>/);
  assert.match(silent, new RegExp(`<h3 id="[^"]+">${PAUSED_UPDATES.title}</h3><p class="ah-ingest-status__sub">${PAUSED_UPDATES.detail}</p>`));
});

test("beside the mark a meter that knows no amount draws nothing, one that knows fills, and a veil keeps its words", () => {
  assert.equal(renderToStaticMarkup(marked(createElement(Meter, { fraction: null }))), "");
  assert.equal(
    renderToStaticMarkup(marked(createElement(Meter, { fraction: 0.25 }))),
    '<div class="ah-meter" aria-hidden="true"><i style="width:25.0%"></i></div>',
  );
  const veil = renderToStaticMarkup(marked(createElement(Veil, { label: "Preparing activity summaries…", progress: null })));
  assert.match(veil, /<span class="ah-veil__label">Preparing activity summaries…<\/span>/);
  assert.doesNotMatch(veil, /data-indeterminate/);
});

test("a conversation's wait is a hold in its column; a load that failed keeps its own face and Try again", () => {
  const waiting = renderToStaticMarkup(marked(createElement(TranscriptLoading, loading())));
  assert.match(waiting, /<div class="tr-loading" data-loading-label="loading" data-marked="">/);
  assert.match(waiting, /<div class="ah-hold" data-place="column" data-face="transcript-loading"/);
  assert.match(waiting, /<i data-mark="column" data-resting="false"><\/i>/);
  assert.match(waiting, /<p class="ah-hold__line">Loading messages…<\/p>/);
  assert.doesNotMatch(waiting, /tr-loading__sketch|tr-loading__meter/);
  const continuing = renderToStaticMarkup(marked(createElement(TranscriptLoading, loading({ waitingForTeam: true, canContinue: true }))));
  assert.match(continuing, /<div class="ah-hold__act"><button type="button">Read available messages<\/button><\/div>/);
  const failed = renderToStaticMarkup(marked(createElement(TranscriptLoading, loading({ error: "the session is gone" }))));
  assert.doesNotMatch(failed, /ah-hold/);
  assert.match(failed, /Try again/);
});
