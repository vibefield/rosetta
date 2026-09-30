import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createActivityDetails } from "../renderer/src/lib/activity-details.ts";

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
  it("selects exactly 30/90 calendar days across DST and includes today", () => {
    const data = calendar("2026-01-04", 371, () => 1);
    const short = createActivityDetails(data, "30d", "2026-03-20");
    assert.equal(short.start, "2026-02-19");
    assert.equal(short.days.length, 30);
    assert.equal(short.total, 30);
    assert.equal(short.days.at(-1).id, "2026-03-20");
    assert.equal(createActivityDetails(data, "90d", "2026-06-20").days.length, 90);
    assert.equal(createActivityDetails(data, "53w", "2027-01-09").days.length, 371);
    assert.equal(short.calendar.weeks[0].label, "Feb");
    assert.ok(short.calendar.weeks.flatMap(week => week.days).filter(day => day.id < short.start || day.id > short.end).every(day => day.value === null));
  });

  it("separates missing, future, invalid and recorded zero days", () => {
    const values = [0, null, NaN, -1, Infinity, 12, 1000];
    const result = createActivityDetails(calendar("2026-09-20", 7, index => values[index]), "53w", "2026-09-25");
    assert.equal(result.total, 12);
    assert.equal(result.recordedDayCount, 2);
    assert.equal(result.activeDayCount, 1);
    assert.equal(result.busiestDay.id, "2026-09-25");
    assert.equal(result.weekdays[0].average, 0);
    assert.equal(result.weekdays[1].average, null);
    assert.equal(result.weekdays[5].average, 12);
    const missing = createActivityDetails(calendar("2026-09-20", 7, () => null), "53w", "2026-09-26");
    assert.equal(missing.total, null);
    assert.equal(missing.busiestDay, null);
    const zero = createActivityDetails(calendar("2026-09-20", 7, () => 0), "53w", "2026-09-26");
    assert.equal(zero.total, 0);
    assert.equal(zero.recordedDayCount, 7);
    assert.equal(zero.busiestDay, null);
  });

  it("reconciles month and weekday totals and divides only by recorded days", () => {
    const result = createActivityDetails(calendar("2026-08-23", 42, index => index === 7 ? null : index), "30d", "2026-09-30");
    assert.equal(result.months.reduce((sum, month) => sum + month.total, 0), result.total);
    assert.equal(result.weekdays.reduce((sum, day) => sum + (day.average ?? 0) * day.recordedDays, 0), result.total);
    assert.equal(result.months.reduce((sum, month) => sum + month.recordedDays, 0), result.recordedDayCount);
    assert.equal(result.busiestDay.id, "2026-09-30");
    assert.equal(result.months.length, 1);
  });

  it("limits streaks to the range and breaks them at unrecorded days or gaps", () => {
    const data = calendar("2026-09-20", 7, index => index === 3 ? null : 5);
    const result = createActivityDetails(data, "53w", "2026-09-26");
    assert.equal(result.longestStreak, 3);
    const sparse = { weeks: [{ id: "2026-09-20", days: [data.weeks[0].days[0], data.weeks[0].days[2]] }] };
    assert.equal(createActivityDetails(sparse, "53w", "2026-09-26").longestStreak, 1);
  });
});
