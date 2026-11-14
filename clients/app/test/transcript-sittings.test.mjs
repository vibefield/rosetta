// §07 — days and sittings. One pill carries both cuts.
//
// Timestamps here are built with `new Date(y, m, d, …)`, so every case is
// viewer-local by construction and the test says the same thing in every
// timezone the app runs in.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  PROMPT_CLUSTER_MS,
  SITTING_PAUSE_MS,
  calendarDayKey,
  dayList,
  sittingCount,
  sittingDayLabel,
  sittings,
  withinPromptCluster,
} from "../../room/src/lib/transcript/index.ts";
// The pill's new names are read off the module whole, so a build without them fails its own rows, not the file.
import * as transcript from "../../room/src/lib/transcript/index.ts";
import { pillText } from "../../room/src/components/transcript/DayPill.tsx";

let seq = 0;
const at = (y, mo, d, h, mi, s = 0) => new Date(y, mo - 1, d, h, mi, s).getTime();

function row(stamp, over = {}) {
  seq += 1;
  return {
    timeline_id: over.timeline_id ?? `t${seq}`,
    session_id: "s1",
    ordinal: seq,
    group: { id: `g${seq}`, kind: over.kind ?? "agent_cycle", derivation: "native" },
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

describe("the thresholds are the measured ones", () => {
  it("fifteen minutes, and five for clustering", () => {
    assert.equal(SITTING_PAUSE_MS, 15 * 60_000);
    assert.equal(PROMPT_CLUSTER_MS, 5 * 60_000);
  });
});

describe("sittings — where the pill cuts", () => {
  it("a session inside one day with no pause is ONE sitting", () => {
    const rows = [
      row(at(2026, 8, 31, 23, 18)),
      row(at(2026, 8, 31, 23, 20)),
      row(at(2026, 8, 31, 23, 30)),
    ];
    const list = sittings(rows, at(2026, 9, 1, 12, 0));
    assert.equal(list.length, 1);
    assert.equal(list[0].isFirst, true);
    assert.equal(list[0].resumedAt, null);
    assert.equal(list[0].pauseMs, null);
    assert.equal(list[0].rows.length, 3);
  });

  it("cuts at a pause of fifteen minutes or more, and states its length", () => {
    const first = at(2026, 9, 1, 0, 12);
    const back = at(2026, 9, 1, 12, 31);
    const list = sittings([row(first), row(back)], at(2026, 9, 1, 16, 0));
    assert.equal(list.length, 2);
    assert.equal(list[1].resumedAt, back);
    assert.equal(list[1].pauseMs, back - first);
    assert.equal(list[1].dayChanged, false);
    assert.equal(list[1].isFirst, false);
  });

  it("does NOT cut one second under the threshold", () => {
    const first = at(2026, 9, 1, 12, 0);
    const list = sittings([row(first), row(first + SITTING_PAUSE_MS - 1)], first);
    assert.equal(list.length, 1);
  });

  it("cuts exactly AT the threshold", () => {
    const first = at(2026, 9, 1, 12, 0);
    const list = sittings([row(first), row(first + SITTING_PAUSE_MS)], first);
    assert.equal(list.length, 2);
    assert.equal(list[1].pauseMs, SITTING_PAUSE_MS);
  });

  it("cuts at a day change even when the pause is short", () => {
    const before = at(2026, 8, 31, 23, 58);
    const after = at(2026, 9, 1, 0, 2);
    const list = sittings([row(before), row(after)], at(2026, 9, 1, 12, 0));
    assert.equal(list.length, 2);
    assert.equal(list[1].dayChanged, true);
    // The pill names the day; there was no silence to report, so no time and
    // no `after …` — two marks for one moment would say it twice.
    assert.equal(list[1].pauseMs, null);
    assert.equal(list[1].resumedAt, null);
  });

  it("carries BOTH cuts in one sitting when both happened", () => {
    const before = at(2026, 8, 31, 23, 18);
    const after = at(2026, 9, 1, 0, 2);
    const list = sittings([row(before), row(after)], at(2026, 9, 1, 12, 0));
    assert.equal(list.length, 2);
    assert.equal(list[1].dayChanged, true);
    assert.equal(list[1].resumedAt, after);
    assert.equal(list[1].pauseMs, 44 * 60_000);
  });

  it("a row with no stamp never cuts a sitting", () => {
    const first = at(2026, 9, 1, 12, 0);
    const list = sittings(
      [row(first), row(null), row(null), row(first + 60_000)],
      first,
    );
    assert.equal(list.length, 1);
    assert.equal(list[0].rows.length, 4);
  });

  it("a stamp that runs backwards is a record artefact, not a silence", () => {
    // Real transcripts do this: a compaction summary lands before the command
    // that caused it. A negative gap must not manufacture a sitting.
    const later = at(2026, 9, 1, 5, 14, 8);
    const earlier = at(2026, 9, 1, 5, 11, 55);
    const list = sittings([row(later), row(earlier)], later);
    assert.equal(list.length, 1);
  });

  it("names its day and dates its own span", () => {
    const start = at(2026, 8, 31, 23, 18);
    const end = at(2026, 8, 31, 23, 20); // inside the window, so still one sitting
    const [sitting] = sittings([row(start), row(end)], at(2026, 9, 1, 12, 0));
    assert.equal(sitting.dayKey, calendarDayKey(start));
    assert.equal(sitting.dayLabel, "Yesterday");
    assert.equal(sitting.startedAt, start);
    assert.equal(sitting.endedAt, end);
  });

  it("says nothing about nothing", () => {
    assert.deepEqual(sittings([], Date.now()), []);
  });

  it("a sitting whose rows are all unstamped cannot name its day", () => {
    const [sitting] = sittings([row(null), row(null)], Date.now());
    assert.equal(sitting.dayKey, null);
    assert.equal(sitting.dayLabel, null);
    assert.equal(sitting.startedAt, null);
  });
});

describe("the day words follow the session list, plus §07's year rule", () => {
  const now = at(2026, 9, 1, 12, 0);
  it("Today, Yesterday, then the weekday and date", () => {
    assert.equal(sittingDayLabel(at(2026, 9, 1, 9, 0), now), "Today");
    assert.equal(sittingDayLabel(at(2026, 8, 31, 23, 18), now), "Yesterday");
    assert.equal(sittingDayLabel(at(2026, 8, 24, 9, 0), now), "Mon 24 Aug");
  });

  it("adds the year only when it is not this one", () => {
    assert.equal(sittingDayLabel(at(2025, 8, 24, 9, 0), now), "24 Aug 2025");
  });
});

describe("the day list the pill opens into", () => {
  it("counts the sittings under each day, in document order", () => {
    const rows = [
      row(at(2026, 8, 31, 23, 18)),
      row(at(2026, 9, 1, 0, 2)), // day + pause
      row(at(2026, 9, 1, 12, 31)), // pause
      row(at(2026, 9, 1, 15, 41)), // pause
    ];
    const list = sittings(rows, at(2026, 9, 1, 16, 0));
    assert.equal(list.length, 4);
    const days = dayList(list);
    assert.deepEqual(
      days.map((day) => [day.dayLabel, day.sittingCount]),
      [
        ["Yesterday", 1],
        ["Today", 3],
      ],
    );
    assert.equal(days[1].firstSittingKey, list[1].key);
  });

  it("leaves a day it cannot name out rather than guessing one", () => {
    assert.deepEqual(dayList(sittings([row(null)], Date.now())), []);
  });

  it("sittingCount is the same figure the session row shows", () => {
    const rows = [row(at(2026, 9, 1, 9, 0)), row(at(2026, 9, 1, 12, 0))];
    assert.equal(sittingCount(rows, at(2026, 9, 1, 16, 0)), 2);
  });
});

describe("the clustering window", () => {
  it("holds inside five minutes and lets go past it", () => {
    assert.equal(withinPromptCluster(1_000, 1_000 + PROMPT_CLUSTER_MS), true);
    assert.equal(withinPromptCluster(1_000, 1_000 + PROMPT_CLUSTER_MS + 1), false);
    assert.equal(withinPromptCluster(null, 1_000), false);
    assert.equal(withinPromptCluster(1_000, null), false);
    assert.equal(withinPromptCluster(2_000, 1_000), false);
  });
});

// §07 — a pill that carries a clock says how far back it is. By the viewer's own calendar days counted from today,
// a pill with a clock says the clock alone today, the weekday and the clock on one of the six days before, and the
// day and the clock earlier (with its year when it is not this one) — where the day turned and inside a day alike.
// A pill with no clock keeps its day alone. The words are made when the document is drawn, so a pill read today and
// looked at tomorrow says tomorrow's words.
describe("a pill that carries a clock says how far back it is", () => {
  // Friday 13 November 2026, 23:00 on the viewer's wall clock.
  const now = at(2026, 11, 13, 23, 0);
  /** The pill of a sitting resumed at 20:21 after a pause of 3h 37m, on the given day. */
  const resumedOn = (y, mo, d, seen = now) => {
    const list = sittings([row(at(y, mo, d, 16, 44)), row(at(y, mo, d, 20, 21))], seen);
    assert.equal(list.length, 2);
    return pillText(list[1]);
  };
  const after = "after 3h 37m";

  it("today: the clock alone (a guard: true before the item too)", () => {
    assert.deepEqual(resumedOn(2026, 11, 13), { lead: "20:21", after });
  });

  it("one of the six days before today: the weekday and the clock", () => {
    assert.deepEqual(resumedOn(2026, 11, 12), { lead: "Thu · 20:21", after });
    assert.deepEqual(resumedOn(2026, 11, 7), { lead: "Sat · 20:21", after });
  });

  it("seven days back: the day and the clock", () => {
    assert.deepEqual(resumedOn(2026, 11, 6), { lead: "Fri 6 Nov · 20:21", after });
  });

  it("another year: the date with its year and the clock; inside the six days the weekday still", () => {
    assert.deepEqual(resumedOn(2025, 11, 6), { lead: "6 Nov 2025 · 20:21", after });
    assert.deepEqual(resumedOn(2025, 12, 30, at(2026, 1, 2, 9, 0)), { lead: "Tue · 20:21", after });
  });

  it("where the day turned, too: a pause across midnight says how far back it resumed", () => {
    const intoToday = sittings([row(at(2026, 11, 12, 23, 50)), row(at(2026, 11, 13, 0, 5))], now);
    assert.equal(intoToday.length, 2);
    assert.deepEqual(pillText(intoToday[1]), { lead: "00:05", after: "after 15m" });
    const intoYesterday = sittings([row(at(2026, 11, 11, 23, 50)), row(at(2026, 11, 12, 0, 5))], now);
    assert.deepEqual(pillText(intoYesterday[1]), { lead: "Thu · 00:05", after: "after 15m" });
    // the cut leaves every sitting inside one calendar day
    for (const sitting of [...intoToday, ...intoYesterday]) {
      assert.equal(new Set(sitting.rows.map((r) => calendarDayKey(r.at))).size, 1);
    }
  });

  it("a pill with no clock keeps its day alone (a guard: true before the item too)", () => {
    const turned = sittings([row(at(2026, 11, 12, 23, 55)), row(at(2026, 11, 13, 0, 0))], now);
    assert.equal(turned.length, 2);
    assert.deepEqual(pillText(turned[0]), { lead: "Yesterday", after: null });
    assert.deepEqual(pillText(turned[1]), { lead: "Today", after: null });
    assert.deepEqual(pillText(sittings([row(at(2026, 11, 6, 9, 0))], now)[0]), {
      lead: "Fri 6 Nov",
      after: null,
    });
    assert.deepEqual(pillText(sittings([row(at(2025, 11, 6, 9, 0))], now)[0]), {
      lead: "6 Nov 2025",
      after: null,
    });
  });

  it("23:59 is today until midnight, and the weekday from it", () => {
    const late = at(2026, 11, 12, 23, 59);
    assert.equal(typeof transcript.clockPillLead, "function", "clockPillLead is exported");
    assert.equal(transcript.clockPillLead(late, at(2026, 11, 12, 23, 59, 30)), "23:59");
    assert.equal(transcript.clockPillLead(late, at(2026, 11, 13, 0, 0)), "Thu · 23:59");
  });

  it("a stamp ahead of now (a clock skew between devices) says today's form, never a negative distance's", () => {
    assert.equal(typeof transcript.clockPillLead, "function", "clockPillLead is exported");
    assert.equal(transcript.clockPillLead(at(2026, 11, 13, 23, 30), now), "23:30");
    assert.equal(transcript.clockPillLead(at(2026, 11, 14, 0, 10), now), "00:10");
    assert.equal(transcript.clockPillLead(at(2026, 11, 21, 8, 0), now), "08:00");
    const skewed = sittings([row(at(2026, 11, 13, 22, 0)), row(at(2026, 11, 14, 0, 10))], now);
    assert.deepEqual(pillText(skewed[1]), { lead: "00:10", after: "after 2h 10m" });
  });

  it("the words are the drawing's: the same rows drawn tomorrow say tomorrow's words", () => {
    const rows = [row(at(2026, 11, 13, 16, 44)), row(at(2026, 11, 13, 20, 21))];
    assert.equal(pillText(sittings(rows, now)[1]).lead, "20:21");
    assert.equal(pillText(sittings(rows, at(2026, 11, 14, 9, 0))[1]).lead, "Fri · 20:21");
    assert.equal(pillText(sittings(rows, at(2026, 11, 20, 9, 0))[1]).lead, "Fri 13 Nov · 20:21");
  });
});
