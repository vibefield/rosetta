import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createDayActivity, createHourlyCurve, hourLabel } from "../../room/src/lib/day-activity.ts";
import { createModelColors } from "../../room/src/lib/model-labels.ts";

describe("selected day hourly activity", () => {
  it("reconciles models and hours without redistributing the day's usage", () => {
    const activity = createDayActivity([
      { hour: 0, model: "claude-opus-5-5", tokens: 10 },
      { hour: 16, model: "claude-opus-5-5", tokens: 20 },
      { hour: 16, model: "claude-opus-5-5", tokens: 3 },
      { hour: 16, model: "gpt-5.4", tokens: 15 },
      { hour: 23, model: "gpt-5.4", tokens: 5 },
    ]);
    assert.equal(activity.total, 53);
    assert.equal(activity.hours.length, 24);
    assert.equal(activity.peakHour, 16);
    assert.equal(activity.hours[16], 38);
    assert.equal(activity.hours[1], 0);
    assert.equal(activity.series[0].total, 33);
    assert.equal(activity.series[0].label, "opus-5-5");
    assert.equal(activity.series[0].values[16], 23);
    assert.equal(activity.series.reduce((sum, model) => sum + model.total, 0), activity.total);
  });
  it("rejects invalid magnitudes and hours and keeps empty usage empty", () => {
    const activity = createDayActivity([-1, 24, 2.5, NaN].map(hour => ({ hour, model: "a", tokens: 5 })).concat(
      [0, -1, Infinity, NaN].map(tokens => ({ hour: 1, model: "a", tokens }))));
    assert.equal(activity.total, 0);
    assert.equal(activity.peakHour, null);
    assert.deepEqual(activity.series, []);
    assert.equal(hourLabel(0), "12 AM");
    assert.equal(hourLabel(12), "12 PM");
    assert.equal(hourLabel(23), "11 PM");
  });
  it("resolves model-color collisions and retains project colors across days", () => {
    const ids = ["claude-opus-5-5", "claude-sonnet-4-5", "gpt-5.4"];
    const colors = createModelColors(ids);
    assert.equal(new Set(Object.values(colors)).size, 3);
    assert.deepEqual(createModelColors(ids.reverse()), colors);
    const day = createDayActivity([{ hour: 0, model: "gpt-5.4", tokens: 10 }], colors);
    assert.equal(day.series[0].color, colors["gpt-5.4"]);
  });
  it("curves pass through every hour without negative dips or invented peaks", () => {
    for (const values of [
      [0, 0, 100, 0, 0, 2, 2, 0],
      [1, 2, 3, 100, 101, 101, 2, 1],
      [100, 20, 19, 18, 2, 0, 0, 0],
    ]) {
      const points = values.map((value, hour) => [hour * 40, value]);
      const path = createHourlyCurve(points);
      assert.ok(path.startsWith(`M${points[0].join(",")}`));
      const segments = [...path.matchAll(/C([^C]+)/g)].map(match =>
        match[1].trim().split(/[\s,]+/).map(Number));
      assert.equal(segments.length, points.length - 1);
      segments.forEach(([x1, y1, x2, y2, x3, y3], index) => {
        const [x0, y0] = points[index];
        assert.deepEqual([x3, y3], points[index + 1], "Each recorded hour is on the curve");
        for (let step = 0; step <= 20; step++) {
          const t = step / 20;
          const u = 1 - t;
          const x = u ** 3 * x0 + 3 * u ** 2 * t * x1 + 3 * u * t ** 2 * x2 + t ** 3 * x3;
          const y = u ** 3 * y0 + 3 * u ** 2 * t * y1 + 3 * u * t ** 2 * y2 + t ** 3 * y3;
          assert.ok(x >= x0 - 1e-8 && x <= x3 + 1e-8, "Time never reverses");
          assert.ok(y >= Math.min(y0, y3) - 1e-8 && y <= Math.max(y0, y3) + 1e-8,
            "Interpolated usage stays between adjacent recorded values");
        }
      });
    }
  });
});
