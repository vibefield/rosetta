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
} from "../renderer/src/lib/transcript/index.ts";

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
