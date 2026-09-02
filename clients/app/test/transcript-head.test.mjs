// §08 — the head of the document, proven where it is arithmetic.
//
// The listeners and the scrollTop fix-up live in `useHeadOfDocument`, which
// needs a DOM; everything they decide with lives here and is proven without
// one. Node strips the types off the .ts import.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  advancePull,
  describeHead,
  EARLIER_FAILED_MESSAGE,
  EARLIER_PAGE_SIZE,
  EARLIER_REWRITTEN_MESSAGE,
  earlierAboveOf,
  earlierState,
  frostOf,
  initialEarlierModel,
  initialPull,
  PULL_DECAY_MS,
  PULL_THRESHOLD_PX,
  pullProgress,
  reduceEarlier,
} from "../renderer/src/lib/transcript/head-state.ts";

/** One row, enough of a TimelineEntry for identity and order. */
function row(id, ordinal) {
  return { timeline_id: id, ordinal };
}

function page(prefix, count, from = 0) {
  return Array.from({ length: count }, (_, i) => row(`${prefix}-${from + i}`, from + i));
}

/** A tail of exactly one page: history above is possible. */
function fullContext(ids = page("t", EARLIER_PAGE_SIZE).map((r) => r.timeline_id)) {
  return { tailIds: new Set(ids), pageSize: EARLIER_PAGE_SIZE };
}

/** A tail shorter than a page: the snapshot IS the whole session. */
const shortContext = { tailIds: new Set(["t-0", "t-1"]), pageSize: EARLIER_PAGE_SIZE };

function run(model, actions, ctx) {
  return actions.reduce((current, action) => reduceEarlier(current, action, ctx), model);
}

// ── the states the head line offers ─────────────────────────────────────────

test("an untried snapshot shorter than a page has nothing above", () => {
  const state = earlierState(initialEarlierModel(), shortContext);
  assert.deepEqual(state, { kind: "none" });
});

test("an untried full-page snapshot offers history above", () => {
  const state = earlierState(initialEarlierModel(), fullContext());
  assert.deepEqual(state, { kind: "idle" });
});

test("a settled null cursor is the top of history", () => {
  const ctx = fullContext();
  const model = run(
    initialEarlierModel(),
    [{ type: "hold" }, { type: "page", seq: 1, entries: [], cursor: null }],
    ctx,
  );
  assert.deepEqual(earlierState(model, ctx), { kind: "none" });
});

// ── idle → fetching → held → landed ─────────────────────────────────────────

test("approach fetches and holds; the reveal lands it", () => {
  const ctx = fullContext();
  const fetching = reduceEarlier(initialEarlierModel(), { type: "hold" }, ctx);
  assert.deepEqual(earlierState(fetching, ctx), { kind: "fetching" });
  assert.equal(fetching.older.length, 0, "approach never inserts");

  const held = reduceEarlier(
    fetching,
    { type: "page", seq: fetching.fetchSeq, entries: page("a", 200), cursor: "c2" },
    ctx,
  );
  assert.deepEqual(earlierState(held, ctx), { kind: "held", count: 200 });
  assert.equal(held.older.length, 0, "a held page is not in the document");

  const landed = reduceEarlier(held, { type: "reveal" }, ctx);
  assert.equal(landed.older.length, 200);
  assert.equal(landed.cursor, "c2");
  assert.deepEqual(landed.landing, { count: 200, seq: 1 });
  assert.deepEqual(earlierState(landed, ctx), { kind: "idle" });
});

test("holding is a no-op while a page is already held or in flight", () => {
  const ctx = fullContext();
  const fetching = reduceEarlier(initialEarlierModel(), { type: "hold" }, ctx);
  assert.equal(reduceEarlier(fetching, { type: "hold" }, ctx), fetching);

  const held = reduceEarlier(
    fetching,
    { type: "page", seq: fetching.fetchSeq, entries: page("a", 200), cursor: "c2" },
    ctx,
  );
  assert.equal(reduceEarlier(held, { type: "hold" }, ctx), held);
});

test("holding is a no-op when there is nothing above", () => {
  const model = initialEarlierModel();
  assert.equal(reduceEarlier(model, { type: "hold" }, shortContext), model);
});

test("a page that repeats the snapshot is not a page", () => {
  // The first fetch has no cursor, so the daemon returns the newest page —
  // exactly what the subscription already delivered. It mints the cursor and
  // nothing more; holding it would promise weight that lands nothing.
  const ctx = fullContext(["t-0", "t-1", "t-2"].concat(page("t", 197, 3).map((r) => r.timeline_id)));
  const tail = [...ctx.tailIds].map((id, i) => row(id, i));
  const fetching = reduceEarlier(initialEarlierModel(), { type: "hold" }, ctx);
  const after = reduceEarlier(
    fetching,
    { type: "page", seq: fetching.fetchSeq, entries: tail, cursor: "c2" },
    ctx,
  );
  assert.equal(after.held, null);
  assert.equal(after.cursor, "c2");
  assert.deepEqual(earlierState(after, ctx), { kind: "idle" });
});

test("the held count is what would land, not the raw page length", () => {
  const ctx = fullContext(["t-0", "t-1"].concat(page("t", 198, 2).map((r) => r.timeline_id)));
  const overlap = [...ctx.tailIds].slice(0, 50).map((id, i) => row(id, i));
  const fetching = reduceEarlier(initialEarlierModel(), { type: "hold" }, ctx);
  const held = reduceEarlier(
    fetching,
    {
      type: "page",
      seq: fetching.fetchSeq,
      entries: [...page("a", 200, 1000), ...overlap],
      cursor: "c2",
    },
    ctx,
  );
  assert.deepEqual(earlierState(held, ctx), { kind: "held", count: 200 });
  const landed = reduceEarlier(held, { type: "reveal" }, ctx);
  assert.equal(landed.landing.count, 200, "the receipt counts rows the document gained");
});

// ── fetching + pull → revealing → landed ────────────────────────────────────

test("a pull before the page becomes revealing and lands on arrival", () => {
  const ctx = fullContext();
  const fetching = reduceEarlier(initialEarlierModel(), { type: "hold" }, ctx);
  const revealing = reduceEarlier(fetching, { type: "reveal" }, ctx);
  assert.deepEqual(earlierState(revealing, ctx), { kind: "revealing" });

  // A second pull meanwhile is ignored, not queued.
  assert.equal(reduceEarlier(revealing, { type: "reveal" }, ctx), revealing);
  assert.equal(reduceEarlier(revealing, { type: "hold" }, ctx), revealing);

  const landed = reduceEarlier(
    revealing,
    { type: "page", seq: revealing.fetchSeq, entries: page("a", 200), cursor: "c2" },
    ctx,
  );
  assert.equal(landed.older.length, 200);
  assert.deepEqual(landed.landing, { count: 200, seq: 1 });
});

test("a reveal with nothing held and nothing in flight does nothing", () => {
  const ctx = fullContext();
  const model = initialEarlierModel();
  assert.equal(reduceEarlier(model, { type: "reveal" }, ctx), model);
});

// ── failure and retry ───────────────────────────────────────────────────────

test("a failed fetch keeps its sentence and takes a retry", () => {
  const ctx = fullContext();
  const fetching = reduceEarlier(initialEarlierModel(), { type: "hold" }, ctx);
  const failed = reduceEarlier(
    fetching,
    { type: "fail", seq: fetching.fetchSeq, message: EARLIER_FAILED_MESSAGE },
    ctx,
  );
  assert.deepEqual(earlierState(failed, ctx), {
    kind: "failed",
    message: EARLIER_FAILED_MESSAGE,
  });

  const again = reduceEarlier(failed, { type: "hold" }, ctx);
  assert.deepEqual(earlierState(again, ctx), { kind: "fetching" });
  assert.equal(again.fetchSeq, failed.fetchSeq + 1);
});

test("an answer stamped for an abandoned fetch is dropped", () => {
  const ctx = fullContext();
  const fetching = reduceEarlier(initialEarlierModel(), { type: "hold" }, ctx);
  const stale = { type: "page", seq: fetching.fetchSeq - 1, entries: page("a", 200), cursor: "c2" };
  assert.equal(reduceEarlier(fetching, stale, ctx), fetching);
  const staleFail = { type: "fail", seq: fetching.fetchSeq - 1, message: "x" };
  assert.equal(reduceEarlier(fetching, staleFail, ctx), fetching);
});

// ── the 410 ─────────────────────────────────────────────────────────────────

test("a dead cursor drops the paged history and says so once", () => {
  const ctx = fullContext();
  const landed = run(
    initialEarlierModel(),
    [
      { type: "hold" },
      { type: "page", seq: 1, entries: page("a", 200), cursor: "c2" },
      { type: "reveal" },
      { type: "hold" },
      { type: "page", seq: 2, entries: page("b", 200), cursor: "c3" },
    ],
    ctx,
  );
  assert.equal(landed.older.length, 200);
  assert.notEqual(landed.held, null);

  const expired = reduceEarlier(landed, { type: "expired", seq: landed.fetchSeq }, ctx);
  assert.deepEqual(expired.older, [], "paged history drops rather than rebasing");
  assert.equal(expired.held, null, "the held page goes with it");
  assert.equal(expired.cursor, undefined, "paging starts again from the head");
  assert.deepEqual(earlierState(expired, ctx), { kind: "idle" });
  assert.equal(expired.notice.message, EARLIER_REWRITTEN_MESSAGE);
  assert.equal(expired.notice.seq, 1);

  const dismissed = reduceEarlier(expired, { type: "clearNotice" }, ctx);
  assert.equal(dismissed.notice, null);
  assert.equal(reduceEarlier(dismissed, { type: "clearNotice" }, ctx), dismissed);
});

test("the 410 sentence stands until the daemon answers again", () => {
  const ctx = fullContext();
  const fetching = reduceEarlier(initialEarlierModel(), { type: "hold" }, ctx);
  const expired = reduceEarlier(fetching, { type: "expired", seq: fetching.fetchSeq }, ctx);
  const retrying = reduceEarlier(expired, { type: "hold" }, ctx);
  assert.equal(retrying.notice.seq, 1, "a retry does not erase the sentence");

  const recovered = reduceEarlier(
    retrying,
    { type: "page", seq: retrying.fetchSeq, entries: page("a", 200), cursor: "c2" },
    ctx,
  );
  assert.equal(recovered.notice, null, "the answer is the sentence's end");
});

// ── the session change ──────────────────────────────────────────────────────

test("changing session drops the held page and orphans the fetch", () => {
  const ctx = fullContext();
  const held = run(
    initialEarlierModel(),
    [
      { type: "hold" },
      { type: "page", seq: 1, entries: page("a", 200), cursor: "c2" },
      { type: "reveal" },
      { type: "hold" },
      { type: "page", seq: 2, entries: page("b", 200), cursor: "c3" },
    ],
    ctx,
  );
  const reset = reduceEarlier(held, { type: "reset" }, ctx);
  assert.deepEqual(reset.older, []);
  assert.equal(reset.held, null);
  assert.equal(reset.cursor, undefined);
  assert.equal(reset.landing, null);
  assert.ok(reset.fetchSeq > held.fetchSeq, "an answer still in flight is orphaned");

  const late = reduceEarlier(reset, { type: "page", seq: held.fetchSeq, entries: page("c", 200), cursor: "c9" }, ctx);
  assert.equal(late, reset, "the previous session's page never lands");
});

test("resetting a pristine model changes nothing", () => {
  const model = initialEarlierModel();
  assert.equal(reduceEarlier(model, { type: "reset" }, fullContext()), model);
});

// ── the accumulator ─────────────────────────────────────────────────────────

test("120px of upward travel at the top fires once", () => {
  let model = initialPull;
  let fired = 0;
  for (const px of [40, 40, 39]) {
    const result = advancePull(model, { travel: px, atTop: true, now: 1_000 }, {});
    model = result.model;
    if (result.fire) fired += 1;
  }
  assert.equal(fired, 0, "119px is not a pull");
  assert.equal(model.travel, PULL_THRESHOLD_PX - 1);

  const last = advancePull(model, { travel: 1, atTop: true, now: 1_000 }, {});
  assert.equal(last.fire, true);
  assert.deepEqual(last.model, initialPull, "firing spends the accumulator");
});

test("downward travel resets the accumulator", () => {
  const armed = advancePull(initialPull, { travel: 90, atTop: true, now: 0 }, {}).model;
  assert.equal(armed.travel, 90);
  const result = advancePull(armed, { travel: -30, atTop: true, now: 10 }, {});
  assert.deepEqual(result.model, initialPull);
  assert.equal(result.fire, false);
});

test("travel is ignored away from the top", () => {
  const armed = advancePull(initialPull, { travel: 90, atTop: true, now: 0 }, {}).model;
  const result = advancePull(armed, { travel: 200, atTop: false, now: 10 }, {});
  assert.equal(result.fire, false, "a scroll that is not at the top never reveals");
  assert.deepEqual(result.model, initialPull);
});

test("stillness decays the accumulator", () => {
  const armed = advancePull(initialPull, { travel: 100, atTop: true, now: 0 }, {}).model;
  const late = advancePull(armed, { travel: 30, atTop: true, now: PULL_DECAY_MS }, {});
  assert.equal(late.fire, false, "the banked 100px is gone");
  assert.equal(late.model.travel, 30);

  const prompt = advancePull(armed, { travel: 30, atTop: true, now: PULL_DECAY_MS - 1 }, {});
  assert.equal(prompt.fire, true, "within the decay window the travel adds up");
});

test("the bar reads the accumulator as a fraction", () => {
  assert.equal(pullProgress(initialPull), 0);
  assert.equal(pullProgress({ travel: 60, at: 0 }), 0.5);
  assert.equal(pullProgress({ travel: 600, at: 0 }), 1);
});

test("a custom threshold is honoured", () => {
  const result = advancePull(initialPull, { travel: 60, atTop: true, now: 0 }, { threshold: 60 });
  assert.equal(result.fire, true);
});

// ── the fade ────────────────────────────────────────────────────────────────

test("--frost rises over the first 24px and clamps", () => {
  assert.equal(frostOf(0), 0);
  assert.equal(frostOf(12), 0.5);
  assert.equal(frostOf(24), 1);
  assert.equal(frostOf(2_400), 1);
  assert.equal(frostOf(-10), 0);
});

// ── the count arithmetic ────────────────────────────────────────────────────

test("earlierAbove is the session total minus what is loaded", () => {
  assert.equal(earlierAboveOf(583, 200), 383);
  assert.equal(earlierAboveOf(583, 400), 183);
  assert.equal(earlierAboveOf(583, 600), 0, "never negative");
  assert.equal(earlierAboveOf(null, 200), null, "no total, no number");
});

// ── the head line ───────────────────────────────────────────────────────────

test("the rest line counts what is above and offers the pull", () => {
  const view = describeHead({ earlier: { kind: "idle" }, above: 383, pull: 0 });
  assert.equal(view.state, "rest");
  assert.equal(view.figure, "383 earlier");
  assert.equal(view.trail, "· pull to load");
  assert.equal(view.fill, "0", "empty means nothing is held yet");
});

test("without a total the word stands alone", () => {
  const view = describeHead({ earlier: { kind: "idle" }, above: null, pull: 0 });
  assert.equal(view.figure, "earlier");
  assert.equal(view.trail, "· pull to load");
});

test("a held page stands the bar full and leaves the width to the stylesheet", () => {
  const view = describeHead({ earlier: { kind: "held", count: 200 }, above: 383, pull: 0 });
  assert.equal(view.state, "held");
  assert.equal(view.fill, null);
});

test("the pull spends held weight and gathers unheld weight", () => {
  const spending = describeHead({ earlier: { kind: "held", count: 200 }, above: 383, pull: 0.25 });
  assert.equal(spending.state, "armed");
  assert.equal(spending.trail, "· keep going");
  assert.equal(spending.fill, "75%");

  const gathering = describeHead({ earlier: { kind: "idle" }, above: 383, pull: 0.25 });
  assert.equal(gathering.state, "armed");
  assert.equal(gathering.fill, "25%");
});

test("a pull that outran the fetch reads loading, with the page size", () => {
  const view = describeHead({ earlier: { kind: "revealing" }, above: 383, pull: 1 });
  assert.equal(view.state, "loading");
  assert.equal(view.lead, "loading");
  assert.equal(view.figure, `${EARLIER_PAGE_SIZE} earlier`);
  assert.equal(view.fill, null, "indeterminate and colourless");
});

test("the start is a fact, and a pull there does nothing", () => {
  const view = describeHead({
    earlier: { kind: "none" },
    above: 0,
    pull: 1,
    startLabel: "Mon 31 Aug · 23:18:12",
  });
  assert.equal(view.state, "start");
  assert.equal(view.lead, "session start ·");
  assert.equal(view.figure, "Mon 31 Aug · 23:18:12");
  assert.equal(view.bar, false, "the bar goes");
});

test("the start says itself even with no stamp to show", () => {
  const view = describeHead({ earlier: { kind: "none" }, above: 0, pull: 0 });
  assert.equal(view.lead, "session start");
  assert.equal(view.figure, null);
});

test("failure is a muted sentence with the next step in the line", () => {
  const view = describeHead({
    earlier: { kind: "failed", message: EARLIER_FAILED_MESSAGE },
    above: 383,
    pull: 0,
  });
  assert.equal(view.state, "error");
  assert.equal(view.lead, "the daemon did not answer ·");
  assert.equal(view.retry, true);
  assert.equal(view.bar, false, "no bar, no red — nothing was destroyed");
});

test("finding states its cause and outranks every other state", () => {
  const view = describeHead({
    earlier: { kind: "held", count: 200 },
    above: 383,
    pull: 0.5,
    finding: "23:18:41",
  });
  assert.equal(view.state, "finding");
  assert.equal(view.figure, "23:18:41");
  assert.equal(view.trail, "· loading earlier");
});
