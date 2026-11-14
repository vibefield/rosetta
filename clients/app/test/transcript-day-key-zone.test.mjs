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
// The pill's new names are read off the module whole, so a build without them fails its own rows, not the file.
import * as transcript from "../../room/src/lib/transcript/index.ts";

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

// §07 — a pill that carries a clock says how far back it is, counted in the viewer's calendar days, so a 23- or
// 25-hour day never moves it; and the wait for the next day, when the room re-reads its clock, is the wall clock's
// own. In 2026 the clocks spring forward on 8 March and fall back on 1 November.
describe("a pill's distance is whole calendar days across a daylight-saving change (America/Los_Angeles)", () => {
  const wall = (y, mo, d, h, mi) => new Date(y, mo - 1, d, h, mi).getTime();

  it("spring forward: 23:30 yesterday is a day back though under 24 hours have passed", () => {
    assert.equal(typeof transcript.clockPillLead, "function", "clockPillLead is exported");
    // 7 March 23:30 PST to 8 March 23:45 PDT is 23h 15m
    assert.equal(transcript.clockPillLead(wall(2026, 3, 7, 23, 30), wall(2026, 3, 8, 23, 45)), "Sat · 23:30");
  });

  it("spring forward: seven days back is the date though under seven days of hours have passed", () => {
    assert.equal(typeof transcript.clockPillLead, "function", "clockPillLead is exported");
    // 1 March 23:30 PST to 8 March 23:00 PDT is 6 days 22h 30m
    assert.equal(
      transcript.clockPillLead(wall(2026, 3, 1, 23, 30), wall(2026, 3, 8, 23, 0)),
      "Sun 1 Mar · 23:30",
    );
  });

  it("fall back: one day of 25 hours is still today at its end", () => {
    assert.equal(typeof transcript.clockPillLead, "function", "clockPillLead is exported");
    // 1 November 00:05 PDT to 23:55 PST is 24h 50m
    assert.equal(transcript.clockPillLead(wall(2026, 11, 1, 0, 5), wall(2026, 11, 1, 23, 55)), "00:05");
  });

  it("the wait for the next day is the wall clock's: 23 hours, 25 hours, and 12 on an ordinary noon", () => {
    assert.equal(typeof transcript.msToNextViewerDay, "function", "msToNextViewerDay is exported");
    const hour = 3_600_000;
    assert.equal(transcript.msToNextViewerDay(wall(2026, 3, 8, 0, 0)), 23 * hour);
    assert.equal(transcript.msToNextViewerDay(wall(2026, 11, 1, 0, 0)), 25 * hour);
    assert.equal(transcript.msToNextViewerDay(wall(2026, 7, 15, 12, 0)), 12 * hour);
    assert.equal(transcript.msToNextViewerDay(wall(2026, 7, 15, 23, 59) + 59_999), 1);
  });
});
