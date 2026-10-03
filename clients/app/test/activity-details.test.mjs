import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createActivityDetails } from "../../room/src/lib/activity-details.ts";

function calendar(start, count, value) {
  const days = Array.from({ length: count }, (_, index) => {
    const date = new Date(`${start}T12:00:00Z`);
    date.setUTCDate(date.getUTCDate() + index);
    const id = date.toISOString().slice(0, 10);
    return { id, value: value(index), label: id };
  });
  return { weeks: Array.from({ length: Math.ceil(count / 7) }, (_, index) => ({
    id: days[index * 7].id, days: days.slice(index * 7, index * 7 + 7),
  })) };
}

describe("expanded activity figures", () => {
  it("keeps the full available calendar history across DST and includes today", () => {
    const data = calendar("2026-01-04", 371, () => 1);
    const result = createActivityDetails(data, "2026-03-20");
    assert.equal(result.start, "2026-01-04");
    assert.equal(result.days.length, 76);
    assert.equal(result.total, 76);
    assert.equal(result.days.at(-1).id, "2026-03-20");
    assert.equal(createActivityDetails(data, "2027-01-09").days.length, 371);
    assert.equal(result.calendar.weeks[0].label, "Jan");
    assert.ok(result.calendar.weeks.flatMap(week => week.days).filter(day => day.id > result.end).every(day => day.value === null));
  });

  it("separates missing, future, invalid and recorded zero days", () => {
    const values = [0, null, NaN, -1, Infinity, 12, 1000];
    const result = createActivityDetails(calendar("2026-09-20", 7, index => values[index]), "2026-09-25");
    assert.equal(result.total, 12);
    assert.equal(result.recordedDayCount, 2);
    assert.equal(result.activeDayCount, 1);
    assert.equal(result.busiestDay.id, "2026-09-25");

    const missing = createActivityDetails(calendar("2026-09-20", 7, () => null), "2026-09-26");
    assert.equal(missing.total, null);
    assert.equal(missing.busiestDay, null);
    const zero = createActivityDetails(calendar("2026-09-20", 7, () => 0), "2026-09-26");
    assert.equal(zero.total, 0);
    assert.equal(zero.recordedDayCount, 7);
    assert.equal(zero.busiestDay, null);
  });

  it("locates actual current and best runs to mark their calendar dates", () => {
    const result = createActivityDetails(calendar("2026-09-20", 11,
      index => [0, 1, 2, 3, 0, null, 1, 2, 0, 1, 1][index]), "2026-09-30");
    assert.deepEqual(result.bestRun, { start: "2026-09-21", end: "2026-09-23", length: 3 });
    assert.deepEqual(result.currentRun, { start: "2026-09-29", end: "2026-09-30", length: 2 });
    assert.equal(result.runs.length, 3);
    assert.equal(createActivityDetails(calendar("2026-09-20", 11,
      index => index === 10 ? 0 : 1), "2026-09-30").currentRun, null);
  });

  it("breaks streaks at unrecorded days or gaps", () => {
    const data = calendar("2026-09-20", 7, index => index === 3 ? null : 5);
    const result = createActivityDetails(data, "2026-09-26");
    assert.equal(result.longestStreak, 3);
    const sparse = { weeks: [{ id: "2026-09-20", days: [data.weeks[0].days[0], data.weeks[0].days[2]] }] };
    assert.equal(createActivityDetails(sparse, "2026-09-26").longestStreak, 1);
  });
});
