// A project's activity series during an import: every metadata delivery
// rebuilds every project's view model, and each used to rebuild 371 day keys
// and hand the selected project's heatmap a new array to redraw. The series
// is now kept per heat array and per viewer day. It must say exactly what the
// full rebuild said, and keep its identity only while its inputs do.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  ACTIVITY_HEATMAP_DAY_COUNT,
  buildProjectViewModelFromSessions,
  getActivityGridStartTimestamp,
  getCalendarDayKey,
} from "../../room/src/lib/model.ts";

const DAY = 86_400_000;
const now = new Date(2026, 9, 31, 12).getTime();
const project = { id: "pa", name: "Project a", root_paths: ["/work/a"], git_repo: null };
const kindOf = () => "claude-code";
const tintOf = () => "#888";
const build = (stats, at = now) => buildProjectViewModelFromSessions(project, [], stats, kindOf, at, tintOf);

/** The series as the room computed it before it was kept: the reference. */
function reference(heat, currentTimestamp) {
  const tokensByDay = new Map(heat.map((day) => [day.day, day.tokens]));
  const start = getActivityGridStartTimestamp(currentTimestamp);
  const today = getCalendarDayKey(currentTimestamp);
  const dailyTokens = [];
  let currentDayIndex = ACTIVITY_HEATMAP_DAY_COUNT - 1;
  for (let index = 0; index < ACTIVITY_HEATMAP_DAY_COUNT; index += 1) {
    const key = getCalendarDayKey(start + index * DAY + DAY / 2);
    if (key === today) currentDayIndex = index;
    dailyTokens.push(index > currentDayIndex ? -1 : (tokensByDay.get(key) ?? 0));
  }
  const last30 = Array.from({ length: 30 }, (_, i) => Math.max(0, dailyTokens[currentDayIndex - (29 - i)] ?? 0));
  return {
    dailyTokens,
    currentDayIndex,
    recentTokenBuckets: Array.from({ length: 15 }, (_, i) => (last30[i * 2] ?? 0) + (last30[i * 2 + 1] ?? 0)),
    tokensLast30Days: last30.reduce((a, b) => a + b, 0),
    tokensLast53Weeks: dailyTokens.reduce((a, b) => a + Math.max(0, b), 0),
  };
}

const facts = (model) => ({
  dailyTokens: [...model.dailyTokens],
  currentDayIndex: model.currentDayIndex,
  recentTokenBuckets: [...model.recentTokenBuckets],
  tokensLast30Days: model.tokensLast30Days,
  tokensLast53Weeks: model.tokensLast53Weeks,
});

function stats(at, entries) {
  return {
    heat: entries.map(([daysAgo, tokens]) => ({
      day: getCalendarDayKey(at - daysAgo * DAY), tokens, messages: 1, cost_micro_usd: tokens, priced_tokens: tokens, unpriced_tokens: 0,
    })),
    contribution: [],
  };
}

describe("a project's activity series", () => {
  it("says what the full rebuild said, with and without figures, across days", () => {
    for (const at of [now, now + 9 * 3_600_000, now + 40 * DAY, new Date(2026, 2, 8, 0, 30).getTime(), new Date(2026, 10, 1, 23, 30).getTime()]) {
      assert.deepEqual(facts(build(undefined, at)), reference([], at), `no figures at ${new Date(at).toISOString()}`);
      const s = stats(at, [[0, 50], [1, 7], [12, 300], [29, 4], [30, 9], [200, 1_000], [400, 5]]);
      assert.deepEqual(facts(build(s, at)), reference(s.heat, at), `figures at ${new Date(at).toISOString()}`);
      // The spend windows still read every heat day, and the last 30 of them.
      const model = build(s, at);
      assert.equal(model.spendLast53Weeks.costMicroUsd, 1_375);
      assert.equal(model.spendLast30Days.costMicroUsd, 50 + 7 + 300 + 4);
    }
  });

  it("keeps its identity while the heat days and the viewer's day do, and only then", () => {
    const s = stats(now, [[0, 50], [3, 20]]);
    const first = build(s);
    const again = build(s, now + 60_000);
    assert.equal(again.dailyTokens, first.dailyTokens, "a delivery a minute later redraws nothing");
    assert.equal(again.recentTokenBuckets, first.recentTokenBuckets);
    // Projects without figures share one series.
    assert.equal(build(undefined).dailyTokens, build(undefined, now + 1).dailyTokens);
    // New figures (a new answer is a new array) and a new day are new series.
    const landed = stats(now, [[0, 50], [3, 20]]);
    assert.notEqual(build(landed).dailyTokens, first.dailyTokens);
    assert.deepEqual(facts(build(landed)), facts(first));
    const tomorrow = build(s, now + DAY);
    assert.notEqual(tomorrow.dailyTokens, first.dailyTokens);
    assert.deepEqual(facts(tomorrow), reference(s.heat, now + DAY));
    // Back to today: the values are today's again.
    assert.deepEqual(facts(build(s)), reference(s.heat, now));
  });
});
