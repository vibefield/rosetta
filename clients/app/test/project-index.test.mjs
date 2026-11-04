// Sessions indexed by project once per metadata change: the room's project
// models and the selected project's thread list must be exactly what the
// O(projects × sessions) filters produced, in the same order. While an import
// runs, the metadata those models are built from lands as a transition.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  UNASSIGNED_PROJECT_ID,
  buildProjectViewModel,
  buildProjectViewModelFromSessions,
  indexSessionsByProject,
  projectKey,
  rootSessionsOfProject,
  sessionsOfProject,
  unassignedProject,
} from "../../room/src/lib/model.ts";
import { compareOrderedSessions } from "../../room/src/lib/activity-order.ts";
import { dataHooks } from "./support/hooks.mjs";

const now = new Date(2026, 9, 31, 12).getTime();
const day = 86_400_000;

function session(id, project_id, over = {}) {
  const at = "at" in over ? over.at : now - day;
  return {
    id,
    native_session_id: `${id}-native`,
    runtime_id: over.runtime_id ?? "rt-claude",
    project_id,
    cwd: null,
    parent_session_id: over.parent ?? null,
    delegation: null,
    root_session_id: over.parent ?? id,
    role: over.parent ? "subagent" : "main",
    title: `Session ${id}`,
    summary: null,
    started_at: 1_000,
    updated_at: 2_000,
    activity: at === null ? null : { first_at: at - 60_000, last_at: at },
    stream_state: over.state ?? "indexed",
  };
}

function project(id) {
  return { id, name: `Project ${id}`, root_paths: [`/work/${id}`], git_repo: null };
}

// Interleaved on purpose: projects, subagents and unassigned sessions arrive
// in no particular order, as a cold import's metadata bursts do.
const sessions = [
  session("a1", "pa", { at: now - 3 * day }),
  session("b1", "pb", { state: "pending", at: null }),
  session("u1", null, { at: now - 2 * day }),
  session("a2", "pa", { at: now - day, runtime_id: "rt-codex" }),
  session("a1-sub", "pa", { parent: "a1", state: "streaming" }),
  session("b2", "pb", { at: now - 5 * day }),
  session("a3", "pa", { state: "streaming", at: now - 4 * day }),
  session("u2", null, { at: null }),
  session("c1", "pc", { at: now }),
];
const rows = [project("pa"), project("pb"), project("pc"), project("pempty"), unassignedProject()];
const kindOf = (runtimeId) => (runtimeId === "rt-codex" ? "codex" : "claude-code");
const tint = (id) => `tint-${id}`;

describe("indexSessionsByProject", () => {
  it("files every session under its projectKey, keeping the input's order", () => {
    const index = indexSessionsByProject(sessions);
    assert.deepEqual([...index.keys()], ["pa", "pb", UNASSIGNED_PROJECT_ID, "pc"]);
    for (const [key, list] of index) {
      assert.deepEqual(
        list.map((s) => s.id),
        sessions.filter((s) => projectKey(s) === key).map((s) => s.id),
      );
    }
    assert.equal([...index.values()].reduce((n, list) => n + list.length, 0), sessions.length);
  });

  it("answers an empty list for a project with no sessions", () => {
    const index = indexSessionsByProject(sessions);
    assert.deepEqual(sessionsOfProject(index, "pempty"), []);
    assert.deepEqual(rootSessionsOfProject(index, "pempty"), []);
    assert.equal(indexSessionsByProject([]).size, 0);
  });

  it("gives each project's top-level threads in the order the old filter did", () => {
    const index = indexSessionsByProject(sessions);
    for (const row of rows) {
      assert.deepEqual(
        rootSessionsOfProject(index, row.id),
        sessions.filter((s) => projectKey(s) === row.id && s.parent_session_id === null),
      );
    }
    // The caller's ranking then sees the same input, so the list is identical.
    const order = { sessions: new Map(), projects: new Map(), bootstrapping: false };
    const ranked = (list) => list.sort((l, r) => compareOrderedSessions(l, r, order)).map((s) => s.id);
    assert.deepEqual(
      ranked(rootSessionsOfProject(index, "pa")),
      ranked(sessions.filter((s) => projectKey(s) === "pa" && s.parent_session_id === null)),
    );
  });

  it("does not share the caller's array: ranking one project's threads leaves the index alone", () => {
    const index = indexSessionsByProject(sessions);
    const before = sessionsOfProject(index, "pa").map((s) => s.id);
    rootSessionsOfProject(index, "pa").reverse();
    assert.deepEqual(sessionsOfProject(index, "pa").map((s) => s.id), before);
  });
});

describe("buildProjectViewModelFromSessions", () => {
  it("builds exactly the view model the all-sessions filter built, for every project", () => {
    const index = indexSessionsByProject(sessions);
    for (const row of rows) {
      const stats = row.id === "pa"
        ? {
            heat: [{ day: "2026-10-30", tokens: 120, messages: 4 }],
            contribution: [{ session_id: "a1", tokens: 300, messages: 7 }],
          }
        : undefined;
      assert.deepEqual(
        buildProjectViewModelFromSessions(row, sessionsOfProject(index, row.id), stats, kindOf, now, tint),
        buildProjectViewModel(row, sessions, stats, kindOf, now, tint),
        row.id,
      );
    }
  });

  it("keeps readiness and counts per project", () => {
    const index = indexSessionsByProject(sessions);
    const of = (id) =>
      buildProjectViewModelFromSessions(
        rows.find((row) => row.id === id), sessionsOfProject(index, id), undefined, kindOf, now, tint,
      );
    assert.equal(of("pa").sessionCount, 4);
    assert.equal(of("pa").rootSessionCount, 3);
    assert.equal(of("pa").readiness, "partial");
    assert.equal(of("pb").readiness, "partial");
    assert.equal(of("pc").readiness, "ready");
    assert.equal(of(UNASSIGNED_PROJECT_ID).sessionCount, 2);
    assert.equal(of("pempty").sessionCount, 0);
  });
});

describe("useMeta during an import", () => {
  it("lands metadata, status and errors as transitions only while asked to", () => {
    let handlers = null;
    let subscriptions = 0;
    const hook = dataHooks({
      subscribeMeta(next) {
        subscriptions += 1;
        handlers = next;
        return { close() {} };
      },
    })("useMeta", { transition: true });
    hook.render();
    const meta = { projects: [], sessions: [session("a1", "pa")], runtimes: [] };
    handlers.onChange(meta, "snapshot");
    handlers.onStatus("live");
    assert.equal(hook.transitions, 2);
    let feed = hook.render();
    assert.equal(feed.meta, meta);
    assert.equal(feed.snapshotVersion, 1);
    assert.equal(feed.status, "live");

    // The import ends: the same subscription now delivers urgently.
    hook.render({ transition: false });
    handlers.onChange({ ...meta, sessions: [] }, "delta");
    handlers.onError({ code: "x", message: "y" });
    assert.equal(hook.transitions, 2);
    feed = hook.render();
    assert.equal(feed.meta.sessions.length, 0);
    assert.equal(feed.snapshotVersion, 1);
    assert.equal(feed.error.code, "x");
    assert.equal(subscriptions, 1);
    hook.close();
  });
});
