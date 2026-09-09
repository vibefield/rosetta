import assert from "node:assert/strict";
import { test } from "node:test";
import { lastActive, dayLabel } from "../renderer/src/lib/format.ts";
import { compareSessionActivity, sessionLastActive } from "../renderer/src/lib/session-activity.ts";
import { buildProjectViewModel } from "../renderer/src/lib/model.ts";
import { createSearchResultsViewModel } from "../renderer/src/lib/search-results.ts";
import { getSessionStatus } from "../renderer/src/components/sessions/session-display.ts";
import { recordedTaskDuration, recordedWorkflowDuration } from "../renderer/src/components/transcript/timeline-meta-state.ts";

const now = new Date(2026, 8, 9, 12).getTime();
const day = 86_400_000;
function session(id, at, metadata = now) {
  return {
    id, native_session_id: id, runtime_id: "rt", project_id: "p", cwd: null,
    parent_session_id: null, root_session_id: id, delegation: null, role: "main",
    title: id, summary: null, started_at: metadata, updated_at: metadata,
    activity: at === null ? null : { first_at: at - 60_000, last_at: at },
    stream_state: "indexed",
  };
}

test("now requires recent conversation evidence and rejects unknown/future times", () => {
  assert.equal(lastActive(now, now), "now");
  assert.equal(lastActive(now - 89_999, now), "now");
  assert.equal(lastActive(now - 90_000, now), "1m");
  assert.equal(lastActive(now - 30 * day, now), "30d");
  assert.equal(lastActive(null, now), "unknown");
  assert.equal(lastActive(NaN, now), "unknown");
  assert.equal(lastActive(now + day, now), "future");
  assert.equal(lastActive(now + 1, now), "future");
  assert.equal(dayLabel(null, now), "Activity unknown");
  assert.notEqual(dayLabel(now + day, now), "Today");
  assert.equal(dayLabel(now, now), "Today");
  assert.equal(dayLabel(now - day, now), "Yesterday");
});

test("session ranking and project recency ignore recently rewritten metadata", () => {
  const stale = session("stale", now - 30 * day);
  const recent = session("recent", now - 1_000, now - day);
  const unknown = session("unknown", null);
  const rows = [stale, unknown, recent];
  assert.deepEqual([...rows].sort(compareSessionActivity).map(row => row.id), ["recent", "stale", "unknown"]);
  assert.equal(sessionLastActive(unknown), null);
  assert.equal(sessionLastActive({ ...unknown, activity: undefined }), null);
  const project = { id: "p", name: "Project", root_paths: [], git_repo: null };
  const vm = buildProjectViewModel(project, rows, undefined, () => "claude-code", now);
  assert.equal(vm.lastActiveMs, recent.activity.last_at);
  assert.equal(buildProjectViewModel(project, [unknown], undefined, () => "claude-code", now).lastActiveMs, null);
  assert.equal(compareSessionActivity(session("a", 10), session("b", 10)) < 0, true);
});

test("search recency tie breaks follow the same conversation time", () => {
  const stale = session("stale", now - 30 * day);
  const recent = session("recent", now - 1_000, now - day);
  const hits = [stale, recent].map(row => ({ session_id: row.id, timeline_id: row.id, score: 1 }));
  const project = { row: { id: "p", name: "Project" } };
  const results = createSearchResultsViewModel(hits, [stale, recent], new Map([["p", project]]));
  assert.deepEqual(results.groups[0].sessions.map(row => row.session.id), ["recent", "stale"]);
});

test("stream state describes indexing without claiming an agent is running or done", () => {
  const stale = { ...session("stale", now - 30 * day), stream_state: "streaming" };
  assert.deepEqual(getSessionStatus(stale), { tone: "orange", isLive: false, label: "history incomplete" });
  assert.deepEqual(getSessionStatus(stale, true), { tone: "orange", isLive: false, label: "indexing" });
  assert.equal(getSessionStatus({ ...stale, stream_state: "indexed" }).label, "history indexed");
  assert.equal(getSessionStatus({ ...stale, stream_state: "pending" }).isLive, false);
});

test("archived unfinished work does not acquire a duration ending now", () => {
  const staleTask = { status: "in_progress", started_at: now - 30 * day, terminal_at: null };
  assert.equal(recordedTaskDuration(staleTask), null);
  assert.equal(recordedTaskDuration({ ...staleTask, status: "completed", terminal_at: staleTask.started_at + 60_000 }), 60_000);
  assert.equal(recordedTaskDuration({ ...staleTask, status: "completed", terminal_at: staleTask.started_at - 1 }), null);
  assert.equal(recordedWorkflowDuration({ status: "running", started_at: now - 30 * day, duration_ms: null }), null);
  assert.equal(recordedWorkflowDuration({ status: "completed", duration_ms: 60_000 }), 60_000);
  assert.equal(recordedWorkflowDuration({ status: "completed", duration_ms: -1 }), null);
});

test("Yesterday follows calendar dates across daylight saving changes", () => {
  const before = process.env.TZ;
  process.env.TZ = "America/Los_Angeles";
  try {
    const fallNow = new Date(2026, 10, 2, 12).getTime();
    assert.equal(dayLabel(new Date(2026, 10, 1, 0, 30).getTime(), fallNow), "Yesterday");
    const springNow = new Date(2026, 2, 9, 12).getTime();
    assert.equal(dayLabel(new Date(2026, 2, 8, 23, 30).getTime(), springNow), "Yesterday");
  } finally {
    if (before === undefined) delete process.env.TZ;
    else process.env.TZ = before;
  }
});
