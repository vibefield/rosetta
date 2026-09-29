import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { documentTurns, sittings, teamChronology } from "../renderer/src/lib/transcript/index.ts";
import { documentPlan } from "../renderer/src/components/transcript/document-plan.ts";
import { activityPlan, activityCuts, summarizeActivity } from "../renderer/src/components/transcript/activity-plan.ts";
import { createDisclosureStore } from "../renderer/src/components/transcript/disclosure-store.ts";
import { entry, inline, shell, tool, file } from "./support/ledger-fixtures.mjs";

const now = new Date(2026, 8, 28, 12).getTime();
const human = (text = "Build it") => entry({ type: "user", text: inline(text), attachments: [] });
const reply = (text = "I’ll check it.") => entry({ type: "agent", text: inline(text), attachments: [], citations: [] });
const notice = value => entry({ type: "notice", notice: value });
const recap = () => entry({ type: "summary", purpose: "compaction", text: inline("Continue with the pane layout.") });
const peer = (channel = "peer", kind = "message", extras = {}) => entry({ type: "collaboration", event: {
  kind, channel, sender: "layout", recipient: "coordinator", team: null, color: null,
  summary: null, body: inline("Report from the worker."), task_id: null, delivery_id: null,
  status: null, authored_at: null, source_session_id: null, source_response_id: null,
  work_start_timeline_id: null, work_end_timeline_id: null, ...extras,
}}, { group: { id: "machine", kind: "parent_invocation", derivation: "native" } });
function sequence(rows) { return rows.map((row, i) => ({ ...row, ordinal: i + 1, at: now + i * 1000 })); }
function project(rows, { watermark = null, partial = false, boundaryRows = rows, shared = new Set() } = {}) {
  const cuts = activityCuts(boundaryRows, watermark);
  const days = sittings(rows, now);
  const cutBefore = new Set([...cuts.cutBefore, ...days.slice(1).map(day => day.rows[0].timeline_id)]);
  const turns = documentTurns(rows, { cutBefore, sharedCollaborationSessionIds: shared });
  return activityPlan(documentPlan(turns, days), { ...cuts, partial });
}
const blocks = (rows, options) => project(rows, options).flatMap(sitting => sitting.blocks);
const groups = (rows, options) => blocks(rows, options).filter(block => block.kind === "activity");

describe("conversation-first activity groups", () => {
  it("groups mixed tool, machine-turn peer, team, thought and context events without crossing primary speech", () => {
    const rows = sequence([human(), reply(), shell("pwd"), peer(), peer("team"),
      entry({ type: "thinking", text: inline("Consider the focus path") }), recap(),
      notice({ type: "mode_changed", from: null, to: "review" }), reply("Agreed."), peer(), human("Ship it")]);
    const result = blocks(rows);
    assert.deepEqual(result.map(block => block.kind), ["prompt", "reply", "activity", "reply", "activity", "prompt"]);
    assert.deepEqual(result[2].rows.map(row => row.timeline_id), rows.slice(2, 8).map(row => row.timeline_id));
    assert.deepEqual(summarizeActivity(result[2].items).nouns.map(noun => noun.text),
      ["1 command", "1 peer message", "1 team message", "1 thought", "2 context events"]);
    assert.equal(result[3].head !== null, true, "the worker-triggered primary response keeps its head");
  });

  it("folds single short, missing-body, stored-body and coordinator messages", () => {
    for (const row of [peer(), peer("coordinator"), peer("team", "assignment"), peer("peer", "message", { body: null }),
      peer("peer", "message", { body: { type: "member", preview: "", byte_len: 9000, asset: { digest: "test" } } })]) {
      const group = groups(sequence([row]));
      assert.equal(group.length, 1);
      assert.equal(group[0].rows.length, 1);
      assert.deepEqual(group[0].rows[0].body, row.body);
    }
  });

  it("does not mistake a lead-to-team message or a parent assignment for human chat", () => {
    const assignment = { ...human("Explore the focus path"), group: { id: "parent", kind: "parent_invocation", derivation: "native" } };
    const rows = sequence([assignment, peer("team", "message", { sender: "coordinator", recipient: "layout" }), reply("Found it.")]);
    assert.deepEqual(blocks(rows).map(block => block.kind), ["activity", "reply"]);
    assert.deepEqual(summarizeActivity(groups(rows)[0].items).nouns.map(noun => noun.text), ["1 assignment", "1 team message"]);
  });

  it("keeps human image-only and primary-agent attachment-only messages as boundaries", () => {
    const attachment = { id: "image", kind: "image", name: null, media_type: "image/png", byte_len: null,
      dimensions: null, source: { type: "unavailable", reason: "not stored" } };
    const rows = sequence([peer(), entry({ type: "user", text: null, attachments: [attachment] }), peer(),
      entry({ type: "agent", text: null, attachments: [attachment], citations: [] }), peer()]);
    assert.deepEqual(blocks(rows).map(block => block.kind), ["activity", "prompt", "activity", "reply", "activity"]);
  });

  it("keeps questions and permission requests visible between separate groups", () => {
    const question = tool({ type: "question", questions: [{ prompt: inline("Ship?"), header: null, options: [], multi_select: false }], answers: [] });
    const rows = sequence([peer(), question, shell("pwd"), notice({ type: "permission_requested", request_id: "approve", scope: "write" }), peer()]);
    assert.deepEqual(blocks(rows).map(block => block.kind), ["activity", "request", "activity", "request", "activity"]);
  });

  it("cuts before gaps and watermarks before tool rows can coalesce", () => {
    const rows = sequence([shell("pwd"), shell("ls"), shell("date"), shell("whoami")]);
    rows[2].ordinal = 6; rows[3].ordinal = 7;
    const result = blocks(rows, { watermark: 1 });
    assert.deepEqual(result.map(block => block.kind), ["activity", "watermark", "activity", "gap", "activity"]);
    assert.equal(result[3].count, 3);
    assert.deepEqual(result.filter(block => block.kind === "activity").map(block => block.rows.length), [1, 1, 2]);
  });

  it("never joins activity across a sitting or calendar-day boundary", () => {
    const rows = sequence([peer(), peer(), peer()]);
    rows[1].at = now + 20 * 60_000;
    rows[2].at = now + 24 * 60 * 60_000;
    const result = project(rows);
    assert.equal(result.length, 3);
    assert.ok(result.every(sitting => sitting.blocks.length === 1 && sitting.blocks[0].kind === "activity"));
  });

  it("retains gaps and watermarks when their delivery copy is deduplicated", () => {
    const rows = sequence([peer(), peer(), peer()]);
    rows[1].ordinal = 5; rows[2].ordinal = 6;
    const cuts = activityCuts(rows, 1, [rows[0], rows[2]]);
    assert.deepEqual([...cuts.gaps], [[rows[2].timeline_id, 3]]);
    assert.equal(cuts.watermarkId, rows[2].timeline_id);
    assert.deepEqual([...cuts.cutBefore], [rows[2].timeline_id]);
  });

  it("counts canonical deliveries once and does not manufacture gaps from private teammate ordinals", () => {
    const root = sequence([human(), shell("pwd"), reply("Done")]);
    root[2].at = now + 10_000;
    const one = { ...peer("team", "message", { delivery_id: "delivered", authored_at: now + 3000 }), session_id: "member", ordinal: 80, at: now + 3000 };
    const copy = { ...one, timeline_id: "recipient-copy", session_id: "member2", ordinal: 120 };
    const next = { ...peer("team", "message", { authored_at: now + 4000 }), session_id: "member", ordinal: 99, at: now + 4000 };
    const chronology = teamChronology(root, new Map([["member", [one, next]], ["member2", [copy]]]));
    const result = blocks(chronology.rows, { boundaryRows: root, shared: new Set(["member", "member2"]) });
    assert.equal(result.some(block => block.kind === "gap"), false);
    const activity = result.find(block => block.kind === "activity");
    assert.equal(activity.rows.length, 3);
    assert.deepEqual(summarizeActivity(activity.items).nouns.map(noun => noun.text), ["1 command", "2 team messages"]);
  });

  it("preserves exact report identity and compaction references rather than inferring reports from text", () => {
    const report = peer("team", "report", { task_id: "task", source_session_id: "member", source_response_id: "r1",
      work_start_timeline_id: "start", work_end_timeline_id: "end" });
    const ordinary = peer("peer", "message", { body: inline("Done. Final report.") });
    const rows = sequence([report, ordinary, recap()]);
    const activity = groups(rows)[0];
    assert.equal(activity.rows[0].body.event.work_end_timeline_id, "end");
    assert.deepEqual(summarizeActivity(activity.items).nouns.map(noun => noun.text), ["1 report", "1 peer message", "1 context event"]);
  });

  it("shows failures, stopped work, running evidence and changed filenames independently of the summary truncation", () => {
    const edit = tool({ type: "file_change", changes: [{ operation: "update", path: file("/demo/Pane.tsx"), destination: null, diff: null, lines_added: 2, lines_removed: 1 }] });
    const rows = sequence([edit, shell("test", { status: "failed" }), peer("team", "report", { status: "failed" }),
      shell("watch", { status: "running" }), shell("stop", { status: "interrupted" }), notice({ type: "runtime_error", kind: "provider", summary: "Unavailable" })]);
    const summary = summarizeActivity(groups(rows)[0].items);
    assert.equal(summary.failed, 3);
    assert.equal(summary.interrupted, 1);
    assert.equal(summary.runningRows.length, 1);
    assert.equal(summary.nouns[0].names.includes("Pane.tsx"), true);
    assert.equal(summary.firstAt, rows[0].at);
    assert.equal(summary.lastAt, rows.at(-1).at);
  });

  it("labels only a clipped first group as loaded activity and keeps an appended run's identity", () => {
    const rows = sequence([peer(), peer(), reply(), peer()]);
    const before = groups(rows, { partial: true });
    assert.deepEqual(before.map(group => group.partial), [true, false]);
    const after = groups([...rows, { ...peer(), ordinal: 5, at: now + 4000 }], { partial: true });
    assert.equal(after[1].key, before[1].key);
    assert.equal(groups(sequence([human(), peer()]), { partial: true })[0].partial, false);
  });

  it("keeps a human command visible while folding its machine output", () => {
    const rows = sequence([human("/compact"), notice({ type: "command_output", output: inline("Compaction completed") }), peer()]);
    const result = blocks(rows);
    assert.deepEqual(result.map(block => block.kind), ["prompt", "activity"]);
    assert.equal(result[0].prompts[0].commandOutput, null);
    assert.deepEqual(result[1].rows.map(row => row.timeline_id), rows.slice(1).map(row => row.timeline_id));
  });

  it("does not absorb command output across a watermark or missing history", () => {
    const rows = sequence([human("/compact"), notice({ type: "command_output", output: inline("Completed") }), peer()]);
    assert.deepEqual(blocks(rows, { watermark: 1 }).map(block => block.kind), ["prompt", "watermark", "activity"]);
    rows[1].ordinal = 4; rows[2].ordinal = 5;
    assert.deepEqual(blocks(rows).map(block => block.kind), ["prompt", "gap", "activity"]);
  });

  it("cuts a settings cluster before its gap without reordering earlier prompts", () => {
    const rows = sequence([human("/model"), notice({ type: "command_output", output: inline("Opus 5 (1M context)") }),
      human("/effort max"), notice({ type: "command_output", output: inline("max") })]);
    rows[2].ordinal = 6; rows[3].ordinal = 7;
    const result = blocks(rows);
    assert.deepEqual(result.map(block => block.kind), ["prompt", "gap", "prompt"]);
    assert.equal(result[0].prompts[0].row.timeline_id, rows[0].timeline_id);
    assert.equal(result[2].prompts[0].row.timeline_id, rows[2].timeline_id);
  });

  it("retains non-prompt bodies that inherit a parent invocation group", () => {
    const rows = sequence([notice({ type: "mode_changed", from: null, to: "review" }), shell("pwd"), reply()])
      .map(row => ({ ...row, group: { id: "invocation", kind: "parent_invocation", derivation: "native" } }));
    const result = blocks(rows);
    assert.deepEqual(result.map(block => block.kind), ["activity", "reply"]);
    assert.deepEqual(result[0].items.map(item => item.kind), ["marker", "tool"]);
  });
});

describe("event-owned disclosure state", () => {
  it("preserves open rows and outer overrides across prepends, merges, and live appends", () => {
    const store = createDisclosureStore();
    store.setRow("s:peer", true);
    store.setGroup(["s:peer", "s:tool"], true, 0);
    assert.equal(store.group(["s:earlier", "s:peer", "s:tool", "s:new"], 0), true);
    store.setGroup(["s:peer", "s:tool", "s:new"], false, 0);
    assert.equal(store.group(["s:earlier", "s:peer", "s:tool", "s:new"], 0), false);
    assert.equal(store.row("s:peer"), true, "closing a group never clears inner expansions");
  });
  it("a room toggle clears outer overrides without clearing bodies or leaking sessions", () => {
    const store = createDisclosureStore();
    store.setGroup(["s:peer"], true, 0); store.setRow("s:peer", true);
    assert.equal(store.group(["s:peer"], 1), undefined);
    assert.equal(store.group(["s:peer"], 2), undefined);
    assert.equal(store.row("s:peer"), true);
    assert.equal(createDisclosureStore().row("s:peer"), undefined);
  });
});
