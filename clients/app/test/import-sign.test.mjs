// R11 — the import's sign, handed to a host that draws it in its own chrome
// (`RoomImportSign`, `importSignOf`): the room's own reading of the import, as
// plain words, figures and fractions. A percentage names its step and counts
// only the sizes it knows (an unsized agent is named beside it); after the
// import the sign stays, at rest, and says what is kept. With the host's sign
// the projects column draws no card.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ProjectsColumn } from "../../room/src/components/projects/ProjectsColumn.tsx";
import { createDemoPorts } from "../../room/src/fixtures/index.ts";
import { agoText, importSignOf } from "../../room/src/lib/import-sign.ts";
import { PAUSED_UPDATES, activityPhase, unsizedText } from "../../room/src/lib/ingest-activity.ts";
import { RoomProvider } from "../../room/src/room-context.tsx";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const { createElement } = fromRoom("react");
const { renderToStaticMarkup } = fromRoom("react-dom/server");

const NOW = 1_800_000_000_000;
const GB = 1_000_000_000;
const runtime = (overrides = {}) => ({ kind: "codex", runtime_id: "rt-codex", label: null, state: "streaming",
  files_total: 100, files_skimmed: 100, sessions_total: 80, bytes_total: 4 * GB, bytes_streamed: GB,
  records: 300, ...overrides });
const antigravity = (overrides = {}) => runtime({ kind: "antigravity", runtime_id: "rt-ag", bytes_total: 0,
  bytes_streamed: 50_000_000, sessions_total: 12, ...overrides });
const feed = (overrides = {}) => ({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime()] },
  canonicalReady: false, searchReadiness: "building", searchProgress: null, connectionError: null,
  startedAt: 1, uptimeMs: 5_000, receivedAt: NOW, ...overrides });
const activityOf = (given) => ({ phase: activityPhase(given), feed: given, footer: "12.0 MB/s", elapsed: "2m 10s elapsed" });
const signOf = (overrides = {}) => {
  const given = feed(overrides);
  return importSignOf(given, activityOf(given), NOW);
};
const storage = (overrides = {}) => ({ record_bytes: 300_000_000, archive_bytes: 0, search_bytes: 0,
  media_bytes: 40_000_000, measured_at: NOW - 2_000, ...overrides });

/* ── reading counts the sizes it knows ────────────────────────────────── */

test("reading's fraction counts the agents whose size is known and names the one that is not", () => {
  const phase = activityPhase(feed({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime(), antigravity()] } }));
  assert.equal(phase.key, "reading");
  assert.equal(phase.fraction, 0.25, "1 GB of the 4 GB known; Antigravity's bytes are not in the fraction");
  assert.equal(phase.total, 4 * GB);
  assert.equal(phase.current, GB + 50_000_000, "the rate still counts every byte read");
  assert.deepEqual(phase.unsized, ["Antigravity"]);
  assert.equal(phase.amount, "1.05 GB processed · Antigravity's size not known yet");
});

test("a build that knows no size draws no fraction, and a finished unsized agent is no longer named", () => {
  const resumed = activityPhase(feed({ ingest: { phase: "backfilling", projects: [],
    runtimes: [runtime({ bytes_total: 0 }), antigravity()] } }));
  assert.equal(resumed.fraction, null);
  assert.deepEqual(resumed.unsized, ["Codex", "Antigravity"]);
  const settled = activityPhase(feed({ ingest: { phase: "backfilling", projects: [],
    runtimes: [runtime(), antigravity({ state: "done" })] } }));
  assert.equal(settled.unsized, undefined);
  assert.equal(settled.amount, "1.05 GB processed");
  assert.equal(unsizedText(["Antigravity"]), "Antigravity's size not known yet");
  assert.equal(unsizedText(["Codex", "Antigravity"]), "Codex and Antigravity: sizes not known yet");
  assert.equal(unsizedText(["A", "B", "C"]), "A, B and C: sizes not known yet");
});

/* ── the sign ─────────────────────────────────────────────────────────── */

test("there is no sign before the index answers or while it waits for consent", () => {
  assert.equal(importSignOf(feed({ receivedAt: null }), null, NOW), null);
  assert.equal(importSignOf(feed({ ingest: { phase: "awaiting_consent", projects: [], runtimes: [] } }), null, NOW), null);
});

test("while it reads, the sign stands on step 2 with the step's own fraction and what each step opens", () => {
  const sign = signOf({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime(), antigravity()] },
    storage: storage(), memory: { footprint_bytes: 2_400_000_000, archive_bytes: 2_100_000_000 } });
  assert.equal(sign.tone, "working");
  assert.equal(sign.step, 2);
  assert.equal(sign.fraction, 0.25);
  assert.equal(sign.eyebrow, "Importing history · step 2 of 4");
  assert.equal(sign.title, "Importing your newest conversations first");
  assert.equal(sign.label, "Importing history · step 2 of 4: Importing your newest conversations first");
  assert.deepEqual(sign.steps.map((step) => step.state), ["done", "current", "later", "later"]);
  assert.deepEqual(sign.steps.map((step) => step.opens),
    ["Projects and sessions", "Open any conversation", "Charts and figures", "Search everything"]);
  assert.equal(sign.steps[0].figure, "92 sessions");
  assert.equal(sign.steps[2].figure, "after reading");
  assert.equal(sign.steps[3].figure, "after the archive");
  assert.deepEqual(sign.steps[1].lines, ["1.00 GB of 4.00 GB read (25%) · Antigravity's size not known yet"]);
  assert.deepEqual(sign.steps[0].lines, [], "only the current step has lines");
  assert.deepEqual(sign.agents.map((agent) => [agent.name, agent.caption, agent.figure, agent.fraction]), [
    ["Codex", "1.00 GB processed", "1.00 GB of 4.00 GB", 0.25],
    ["Antigravity", "50.0 MB processed · size not known yet", "50.0 MB so far", null],
  ]);
  assert.equal(sign.footer, "12.0 MB/s");
  assert.equal(sign.elapsed, "2m 10s elapsed");
});

test("the space says the record kept for good and the archive in memory until step 3 writes it", () => {
  const building = signOf({ storage: storage(), memory: { footprint_bytes: 2_400_000_000, archive_bytes: 2_100_000_000 } });
  const parts = Object.fromEntries(building.space.parts.map((part) => [part.key, part]));
  assert.equal(building.space.read, "1.00 GB of 4.00 GB", "every size known while it reads");
  assert.equal(parts.record.note, "3.3× smaller than read · kept for good");
  assert.equal(parts.record.size, "300.0 MB");
  assert.equal(parts.archive.note, "in memory now · written to disk at step 3");
  assert.equal(parts.archive.size, "2.10 GB");
  assert.deepEqual([parts.archive.disk, parts.archive.memory], [0, 2_100_000_000]);
  assert.equal(parts.search.note, "built at step 4");
  assert.equal(parts.search.size, "—");
  assert.equal(building.space.kept, "340.0 MB");
  assert.equal(building.space.inMemory, "2.10 GB");
  assert.equal(building.space.memory, "2.40 GB");
  assert.equal(building.space.measuredAt, NOW - 2_000);
  // No measurement yet, or an index that does not say: no space at all.
  assert.equal(signOf().space, null);
});

test("a paused import stands on the step it paused in, at rest; a failure and a failed search ask for attention", () => {
  const paused = signOf({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime()], run: { state: "paused" } } });
  assert.equal(paused.tone, "paused");
  assert.equal(paused.step, 2, "the run's words say it paused; the step under them is reading");
  assert.equal(paused.eyebrow, "Paused · step 2 of 4");
  assert.equal(paused.title, "History import paused");
  assert.deepEqual(paused.steps[1].lines, ["Saved progress is retained. Resume when you are ready."]);
  const failed = signOf({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime()],
    run: { state: "failed", error: "There isn't enough free space on this Mac." } } });
  assert.equal(failed.tone, "attention");
  assert.equal(failed.attention, "import");
  assert.equal(failed.detail, "There isn't enough free space on this Mac.");
  const search = signOf({ ingest: { phase: "live", projects: [], runtimes: [runtime({ state: "done" })] },
    canonicalReady: true, searchReadiness: "failed" });
  assert.equal(search.tone, "attention");
  assert.equal(search.attention, "search");
  assert.equal(search.step, 4);
});

test("while the service does not answer the sign rests and says so, in the card's words", () => {
  const silent = signOf({ connectionError: "connection refused" });
  assert.equal(silent.tone, "paused");
  assert.equal(silent.title, PAUSED_UPDATES.title);
  assert.equal(silent.detail, PAUSED_UPDATES.detail);
  assert.equal(silent.eyebrow, "No updates · step 2 of 4");
  assert.equal(silent.count, null);
});

test("search ready is done: every step done and the fraction whole", () => {
  const ready = signOf({ ingest: { phase: "live", projects: [], runtimes: [runtime({ state: "done" })] },
    canonicalReady: true, searchReadiness: "ready" });
  assert.equal(ready.tone, "done");
  assert.equal(ready.title, "Search ready");
  assert.equal(ready.fraction, 1);
  assert.deepEqual(ready.steps.map((step) => step.state), ["done", "done", "done", "done"]);
  assert.equal(ready.eyebrow, "History · all set");
});

test("after the import the sign stays at rest: up to date, what is kept, when each agent last brought anything", () => {
  const given = feed({
    ingest: { phase: "live", projects: [], runtimes: [runtime({ state: "done", bytes_streamed: 4 * GB })] },
    canonicalReady: true, searchReadiness: "ready",
    storage: storage({ archive_bytes: 4_700_000_000, search_bytes: 3_400_000_000 }),
    memory: { footprint_bytes: 440_000_000, archive_bytes: null },
    live: [
      { kind: "codex", runtime_id: "rt-codex", label: null, ticks: 9, events: 3, polls: 2, rescans: 0,
        admissions: 1, live_rows: 4, last_activity_ms: NOW - 3 * 60_000 },
      { kind: "pi", runtime_id: "rt-pi", label: null, ticks: 9, events: 0, polls: 2, rescans: 0,
        admissions: 0, live_rows: 0, last_activity_ms: null },
    ],
  });
  const rest = importSignOf(given, null, NOW);
  assert.equal(rest.tone, "rest");
  assert.equal(rest.title, "Up to date");
  assert.equal(rest.eyebrow, "History · up to date");
  assert.equal(rest.detail, "Watching 2 agents. New conversations appear here as they happen.");
  assert.equal(rest.elapsed, null);
  assert.equal(rest.footer, "Last new activity 3m ago");
  assert.deepEqual(rest.agents.map((agent) => [agent.name, agent.state, agent.lastNew]),
    [["Codex", "done", "3m ago"], ["Pi", "live", null]]);
  const archive = rest.space.parts.find((part) => part.key === "archive");
  assert.equal(archive.note, "what you browse · rebuilt from the record");
  assert.equal(rest.space.parts.find((part) => part.key === "search").note, "rebuilt from the archive");
  assert.equal(rest.space.inMemory, null);
  assert.equal(rest.space.read, "4.00 GB", "what was read, with no total once the import is done");
  // An index that started warm read nothing this start.
  const warm = importSignOf({ ...given, ingest: { phase: "live", projects: [], runtimes: [] } }, null, NOW);
  assert.equal(warm.space.read, null);
  assert.equal(warm.space.parts[0].note, "kept for good");
});

test("ago is coarse", () => {
  assert.equal(agoText(null, NOW), null);
  assert.equal(agoText(NOW - 10_000, NOW), "just now");
  assert.equal(agoText(NOW - 5 * 60_000, NOW), "5m ago");
  assert.equal(agoText(NOW - 2 * 3_600_000, NOW), "2h ago");
  assert.equal(agoText(NOW - 3 * 86_400_000, NOW), "3d ago");
});

/* ── the host's sign retires the card ───────────────────────────────── */

test("with the host's sign the projects column draws no import card", () => {
  const nothing = () => {};
  const column = (importCard) => renderToStaticMarkup(createElement(RoomProvider,
    { ports: createDemoPorts(), options: { storage: null } },
    createElement(ProjectsColumn, {
      projects: [], activityOrder: { sessions: new Map(), projects: new Map(), bootstrapping: true },
      selectedProjectId: null, sort: "recent", onSortChange: nothing, homeDirectory: null, currentTimestamp: NOW,
      ingest: feed().ingest, activity: activityOf(feed()), onProjectSelect: nothing,
      ...(importCard === undefined ? {} : { importCard }),
    })));
  assert.match(column(undefined), /class="ah-ingest-status"/, "the card, as rosetta's app shows it");
  assert.doesNotMatch(column(false), /ah-ingest-status|ah-ingest-fold/);
});
