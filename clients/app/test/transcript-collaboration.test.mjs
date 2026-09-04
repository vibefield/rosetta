// §04A — ordinary Agent workers stay invocation-rooted; Agent Team boundary
// events enter shared chronology and carry address/runtime facts.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  documentTurns,
  interleaveTeamCollaboration,
  participantPresentation,
  runtimeProfile,
  taskBoundedReportWork,
  teamMemberPresentations,
  teamParticipants,
  teamSessionIds,
  workerThreadLinks,
} from "../renderer/src/lib/transcript/index.ts";

const inline = (text) => ({ type: "inline", text });

function entry(id, sessionId, ordinal, at, body, over = {}) {
  return {
    timeline_id: id,
    session_id: sessionId,
    ordinal,
    group: {
      id: over.groupId ?? id,
      kind: over.kind ?? "agent_cycle",
      derivation: "native",
    },
    response: over.responseId ? { id: over.responseId, derivation: "native" } : null,
    block_index: null,
    native_id: null,
    parent_native_id: null,
    at,
    model: over.model ?? null,
    effort: over.effort ?? null,
    body,
    parent_tool_native_id: null,
    usage: {
      input: null,
      output: null,
      cache_read: null,
      cache_write: null,
      thinking: null,
    },
    provenance: { agent_version: null, src_cursor: null, src_offset: null },
  };
}

function session(id, parent, role, title) {
  return {
    id,
    native_session_id: `native-${id}`,
    runtime_id: "claude",
    project_id: "project",
    cwd: "/work",
    parent_session_id: parent,
    root_session_id: parent ?? id,
    role,
    title,
    summary: null,
    started_at: 1,
    updated_at: 2,
    stream_state: "indexed",
  };
}

function collaboration(kind, channel, body, over = {}) {
  return {
    type: "collaboration",
    event: {
      kind,
      channel,
      sender: over.sender ?? null,
      recipient: over.recipient ?? null,
      team: over.team ?? null,
      color: over.color ?? null,
      summary: over.summary ?? null,
      body: body === null ? null : inline(body),
      task_id: over.taskId ?? null,
      delivery_id: over.deliveryId ?? null,
      status: over.status ?? null,
    },
  };
}

function delegate(id, childId, name, over = {}) {
  return entry(id, "parent", over.ordinal ?? 1, over.at ?? 100, {
    type: "tool_use",
    tool: {
      correlation: { key: id, derivation: "native_id" },
      native: { id, namespace: null, name: "Agent", provider: null },
      status: { value: "succeeded", basis: "native_terminal_record" },
      action: {
        type: "delegate",
        agent_kind: "general-purpose",
        name,
        model: over.model ?? null,
        mode: null,
        team: over.team === undefined ? "team-a" : over.team,
        color: over.color ?? null,
        objective: inline(over.objective ?? `work assigned to ${name}`),
        child_session_id: childId,
        resume_id: over.resumeId ?? null,
        background: true,
        orphan_result_summary: null,
      },
      failure: null,
      timing: null,
    },
  });
}

describe("Agent Team view facts", () => {
  it("identifies only proven team children", () => {
    const children = [
      session("worker", "parent", "subagent", "Explore"),
      session("member", "parent", "team", "w1-rust-tools"),
    ];
    assert.deepEqual(teamSessionIds("parent", children, []), ["member"]);
    assert.deepEqual(
      teamSessionIds("parent", children, [delegate("d1", "worker", "w2-docs")]),
      ["member", "worker"],
    );
  });

  it("uses observed response profiles before configured fallback", () => {
    const observed = [
      entry("a1", "member", 1, 100, { type: "agent", text: inline("one"), attachments: [], citations: [] }, {
        responseId: "r1", model: "claude-opus-5", effort: "max",
      }),
      entry("a2", "member", 2, 101, { type: "thinking", text: inline("same response") }, {
        responseId: "r1", model: "claude-opus-5", effort: "max",
      }),
      entry("a3", "member", 3, 200, { type: "agent", text: inline("two"), attachments: [], citations: [] }, {
        responseId: "r2", model: "claude-sonnet-5", effort: "high",
      }),
    ];
    assert.deepEqual(runtimeProfile(observed, "opus"), {
      basis: "varies",
      label: "varies · 2 response profiles",
      model: null,
      effort: null,
      profileCount: 2,
    });
    assert.equal(runtimeProfile([], "opus").label, "configured opus · effort unknown");
    assert.equal(runtimeProfile([], null).label, "runtime not recorded");
  });

  it("classifies the first member, later join, and exact resume separately", () => {
    const rows = [
      delegate("d1", "m1", "w1", { ordinal: 1 }),
      delegate("d2", "m2", "w2", { ordinal: 2 }),
      delegate("d3", "m1", "w1", { ordinal: 3, resumeId: "m1" }),
    ];
    const sessions = new Map([
      ["m1", session("m1", "parent", "team", "w1")],
      ["m2", session("m2", "parent", "team", "w2")],
    ]);
    const members = teamMemberPresentations(rows, sessions, new Map());
    assert.deepEqual(
      [...members.values()].map((member) => member.operation),
      ["start", "add", "resume"],
    );
  });

  it("uses the team address when two teams reuse a member name", () => {
    const rows = [
      delegate("d1", "m1", "worker", { ordinal: 1, team: "alpha", color: "blue" }),
      delegate("d2", "m2", "worker", { ordinal: 2, team: "beta", color: "green" }),
    ];
    const sessions = new Map([
      ["m1", session("m1", "parent", "team", "worker")],
      ["m2", session("m2", "parent", "team", "worker")],
    ]);
    const participants = teamParticipants(
      teamMemberPresentations(rows, sessions, new Map()),
    );
    assert.equal(participantPresentation(participants, "worker", "alpha")?.color, "blue");
    assert.equal(participantPresentation(participants, "worker", "beta")?.color, "green");
    assert.equal(participantPresentation(participants, "worker"), null);
  });
});

describe("team boundary chronology", () => {
  it("interleaves by authored time, excludes private coordinator context, and dedupes only ids", () => {
    const parent = [
      entry("p1", "parent", 1, 100, { type: "agent", text: inline("start"), attachments: [], citations: [] }),
      entry("p2", "parent", 2, 300, { type: "agent", text: inline("end"), attachments: [], citations: [] }),
    ];
    const child = [
      entry("private", "member", 1, 150, collaboration("assignment", "coordinator", "private")),
      entry("followup", "member", 2, 175, collaboration("message", "coordinator", "please verify")),
      entry("shared", "member", 3, 200, collaboration("message", "peer", "hello", { sender: "w2" })),
      entry("same-prose", "member", 4, 210, collaboration("message", "peer", "hello", { sender: "w2" })),
    ];
    assert.deepEqual(
      interleaveTeamCollaboration(parent, new Map([["member", child]])).map((row) => row.timeline_id),
      ["p1", "followup", "shared", "same-prose", "p2"],
    );
  });

  it("chooses a duplicate delivery representative deterministically", () => {
    const parent = [
      entry("p1", "parent", 1, 100, {
        type: "agent",
        text: inline("start"),
        attachments: [],
        citations: [],
      }),
    ];
    const laterMapEntry = entry(
      "z-copy",
      "z-member",
      1,
      200,
      collaboration("message", "team", "same delivery", { deliveryId: "delivery-1" }),
    );
    const stableWinner = entry(
      "a-copy",
      "a-member",
      1,
      200,
      collaboration("message", "team", "same delivery", { deliveryId: "delivery-1" }),
    );
    assert.deepEqual(
      interleaveTeamCollaboration(
        parent,
        new Map([
          ["z-member", [laterMapEntry]],
          ["a-member", [stableWinner]],
        ]),
      ).map((row) => row.timeline_id),
      ["p1", "a-copy"],
    );
  });

  it("keeps machine context in a worker prompt but team speech in the shared turn", () => {
    const workerMessage = entry(
      "followup",
      "worker",
      1,
      100,
      collaboration("message", "coordinator", "check resumes", { sender: "coordinator" }),
      { kind: "parent_invocation" },
    );
    const [workerTurn] = documentTurns([workerMessage]);
    assert.equal(workerTurn.prompt.machine.label, "Coordinator follow-up");
    assert.equal(workerTurn.prompt.text, "check resumes");

    const opening = entry("p1", "parent", 1, 50, {
      type: "user",
      text: inline("go"),
      attachments: [],
    }, { kind: "human_turn" });
    const teamMessage = entry(
      "peer",
      "member",
      1,
      100,
      collaboration("message", "peer", "schema is ready", { sender: "w2" }),
      { kind: "parent_invocation" },
    );
    const [sharedTurn] = documentTurns([opening, teamMessage], {
      sharedCollaborationSessionIds: new Set(["member"]),
    });
    assert.equal(sharedTurn.prompt.text, "go");
    assert.deepEqual(sharedTurn.segments.map((segment) => segment.kind), ["collaboration"]);
  });

  it("offers nested report work only for an exact assignment task relation", () => {
    const assignment = entry(
      "assignment",
      "member",
      1,
      100,
      collaboration("assignment", "coordinator", "Fix the model", { taskId: "7" }),
    );
    const action = delegate("action", "nested", "explorer", {
      ordinal: 2,
      at: 150,
      team: null,
    });
    action.session_id = "member";
    const report = entry(
      "report",
      "member",
      3,
      200,
      collaboration("report", "team", "Done", { taskId: "7", sender: "w1" }),
    );

    const work = taskBoundedReportWork(report, [assignment, action, report]);
    assert.deepEqual(work?.rows.map((row) => row.timeline_id), ["assignment", "action"]);
    assert.equal(work?.actionCount, 1);
    const deliveredCopy = entry(
      "parent-report",
      "parent",
      8,
      200,
      collaboration("report", "team", "Done", { taskId: "7", sender: "w1" }),
    );
    assert.deepEqual(
      taskBoundedReportWork(deliveredCopy, [assignment, action])?.rows.map(
        (row) => row.timeline_id,
      ),
      ["assignment", "action"],
    );
    const unmatched = entry(
      "unmatched",
      "member",
      4,
      250,
      collaboration("report", "team", "Done", { taskId: "8" }),
    );
    assert.equal(taskBoundedReportWork(unmatched, [assignment, action, report, unmatched]), null);
  });
});

describe("ordinary Agent worker ownership", () => {
  it("mounts the first invocation once and makes a resume reference that anchor", () => {
    const first = delegate("d1", "worker", "explorer", { ordinal: 1, team: null });
    const resumed = delegate("d2", "worker", "explorer", {
      ordinal: 2,
      team: null,
      resumeId: "worker",
    });
    const lines = documentTurns([first, resumed]).flatMap((turn) =>
      turn.segments.flatMap((segment) => (segment.kind === "ledger" ? segment.lines : [])),
    );
    const worker = session("worker", "parent", "subagent", "Explore");
    const links = workerThreadLinks(
      lines,
      "parent",
      new Map([[worker.id, worker]]),
      new Set(),
    );

    assert.equal(links.anchorRowByChild.get("worker"), "d1");
    assert.deepEqual(
      links.childrenByRow.get("d1")?.map((child) => child.id),
      ["worker"],
    );
    assert.deepEqual(links.referencedChildIdsByRow.get("d2"), ["worker"]);
    assert.equal(lines[0].verb, "Delegated");
    assert.equal(lines[1].verb, "Resumed");
  });

  it("never gives a team child a second, worker-style disclosure", () => {
    const row = delegate("d1", "member", "w1", { ordinal: 1 });
    const lines = documentTurns([row]).flatMap((turn) =>
      turn.segments.flatMap((segment) => (segment.kind === "ledger" ? segment.lines : [])),
    );
    const member = session("member", "parent", "team", "w1");
    const links = workerThreadLinks(
      lines,
      "parent",
      new Map([[member.id, member]]),
      new Set([member.id]),
    );
    assert.equal(links.anchorRowByChild.size, 0);
    assert.equal(links.childrenByRow.size, 0);
  });
});
