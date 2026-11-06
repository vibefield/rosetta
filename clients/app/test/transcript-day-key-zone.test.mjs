// §07 — a stamp's day is its own, on either side of a daylight-saving change.
//
// The zone is pinned to one that keeps daylight saving, so these rows fail the
// same way on any host and in any season: of the two stamps below, one is
// always across a change from today. Node re-reads the zone when TZ is
// assigned, and the day key reads the offset when it is called, so pinning it
// after the imports is enough.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { calendarDayKey, sittings } from "../../room/src/lib/transcript/index.ts";

process.env.TZ = "America/Los_Angeles";

// 00:30 on 15 July, Pacific Daylight Time (UTC−7).
const SUMMER_JUST_AFTER_MIDNIGHT = Date.UTC(2026, 6, 15, 7, 30);
// 23:30 on 15 January, Pacific Standard Time (UTC−8).
const WINTER_JUST_BEFORE_MIDNIGHT = Date.UTC(2026, 0, 16, 7, 30);

function row(stamp, n) {
  return {
    timeline_id: `t${n}`,
    session_id: "s1",
    ordinal: n,
    group: { id: `g${n}`, kind: "agent_cycle", derivation: "native" },
    response: null,
    block_index: null,
    native_id: null,
    parent_native_id: null,
    at: stamp,
    model: null,
    effort: null,
    body: { type: "agent", text: { type: "inline", text: "x" }, attachments: [], citations: [] },
    parent_tool_native_id: null,
    usage: { input: null, output: null, cache_read: null, cache_write: null, thinking: null },
    provenance: { agent_version: null, src_cursor: null, src_offset: null },
  };
}

describe("a day key is the stamp's own day, whatever today's offset (America/Los_Angeles)", () => {
  it("the zone is pinned: the two stamps read 00:30 and 23:30 on the wall clock", () => {
    assert.equal(new Date(SUMMER_JUST_AFTER_MIDNIGHT).getHours(), 0);
    assert.equal(new Date(WINTER_JUST_BEFORE_MIDNIGHT).getHours(), 23);
  });

  it("a summer stamp just after midnight keys to its own day", () => {
    assert.equal(calendarDayKey(SUMMER_JUST_AFTER_MIDNIGHT), "2026-07-15");
  });

  it("a winter stamp just before midnight keys to its own day", () => {
    assert.equal(calendarDayKey(WINTER_JUST_BEFORE_MIDNIGHT), "2026-01-15");
  });

  it("a sitting names the day its first row was written, in either season", () => {
    const now = Date.UTC(2026, 6, 20, 19, 0);
    const [summer] = sittings([row(SUMMER_JUST_AFTER_MIDNIGHT, 1)], now);
    const [winter] = sittings([row(WINTER_JUST_BEFORE_MIDNIGHT, 2)], now);
    assert.equal(summer.dayKey, "2026-07-15");
    assert.equal(winter.dayKey, "2026-01-15");
  });

  it("two rows four minutes apart across midnight are two days, in either season", () => {
    const summerBefore = Date.UTC(2026, 6, 15, 6, 58); // 23:58 on 14 July, PDT
    const winterAfter = Date.UTC(2026, 0, 16, 8, 2); // 00:02 on 16 January, PST
    assert.notEqual(calendarDayKey(summerBefore), calendarDayKey(SUMMER_JUST_AFTER_MIDNIGHT));
    assert.notEqual(calendarDayKey(WINTER_JUST_BEFORE_MIDNIGHT), calendarDayKey(winterAfter));
    assert.equal(calendarDayKey(summerBefore), "2026-07-14");
    assert.equal(calendarDayKey(winterAfter), "2026-01-16");
  });
});
