// §03 — the document as turns. One prompt, one head, ordered segments.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  documentTurns,
  attachmentMentions,
  objectText,
  readAttachments,
} from "../renderer/src/lib/transcript/index.ts";

/* ── factories ───────────────────────────────────────────────────────────── */

let seq = 0;
const inline = (text) => ({ type: "inline", text });

function entry(body, over = {}) {
  seq += 1;
  return {
    timeline_id: over.timeline_id ?? `t${seq}`,
    session_id: "s1",
    ordinal: over.ordinal ?? seq,
    group: { id: over.groupId ?? `g${seq}`, kind: over.kind ?? "agent_cycle", derivation: "native" },
    response: over.responseId ? { id: over.responseId, derivation: "native" } : null,
    block_index: null,
    native_id: null,
    parent_native_id: null,
    at: "at" in over ? over.at : 1_700_000_000_000,
    model: over.model ?? null,
    effort: over.effort ?? null,
    body,
    parent_tool_native_id: null,
    usage: over.usage ?? {
      input: null,
      output: null,
      cache_read: null,
      cache_write: null,
      thinking: null,
    },
    provenance: { agent_version: null, src_cursor: null, src_offset: null },
  };
}

const prompt = (text, over = {}) =>
  entry({ type: "user", text: text === null ? null : inline(text), attachments: over.attachments ?? [] }, {
    kind: "human_turn",
    ...over,
  });

const agent = (text, over = {}) =>
  entry({ type: "agent", text: inline(text), attachments: [], citations: over.citations ?? [] }, over);

const thinking = (text, over = {}) => entry({ type: "thinking", text: inline(text) }, over);

const notice = (n, over = {}) => entry({ type: "notice", notice: n }, { kind: "synthetic", ...over });

const summary = (text, over = {}) =>
  entry({ type: "summary", purpose: over.purpose ?? "session", text: inline(text) }, over);

const shell = (command, over = {}) =>
  entry(
    {
      type: "tool_use",
      tool: {
        correlation: { key: `k${seq}`, derivation: "native_id" },
        native: { id: null, namespace: null, name: "Bash", provider: null },
        status: { value: over.status ?? "succeeded", basis: "native_state" },
        action: {
          type: "shell",
          command: inline(command),
          cwd: null,
          timeout_ms: null,
          background: false,
          outcome: null,
        },
        failure: null,
        timing: null,
      },
    },
    over,
  );

const delegate = (childSessionId, over = {}) =>
  entry(
    {
      type: "tool_use",
      tool: {
        correlation: { key: `k${seq}`, derivation: "native_id" },
        native: { id: null, namespace: null, name: "Task", provider: null },
        status: { value: "succeeded", basis: "native_state" },
        action: {
          type: "delegate",
          agent_kind: "explore",
          name: "Explore",
          model: null,
          mode: null,
          objective: inline("find every consumer of CARD_RADIUS"),
          child_session_id: childSessionId,
          resume_id: null,
          background: false,
          orphan_result_summary: null,
        },
        failure: null,
        timing: null,
      },
    },
    over,
  );

/* ── the mixed sequence ──────────────────────────────────────────────────── */

describe("documentTurns — the anatomy of a turn", () => {
  const rows = [
    prompt("fix the fillet first, then the shadow", { at: 1_000, timeline_id: "p1" }),
    agent("Several real issues visible. Let me fix them.", {
      at: 2_000,
      model: "claude-opus-5",
      effort: "max",
      responseId: "r1",
      usage: { input: 40_000, output: 1_200, cache_read: 9, cache_write: 9, thinking: null },
    }),
    thinking("The plate is 18px against a 19px board gap.", { at: 3_000, responseId: "r1" }),
    shell("pnpm typecheck", { at: 4_000, responseId: "r1" }),
    shell("pnpm test", { at: 5_000, responseId: "r1" }),
    agent("Both clean.", { at: 6_000, responseId: "r1" }),
    notice({ type: "compaction", reason: null }, { at: 7_000 }),
    delegate("child-1", { at: 8_000, responseId: "r1" }),
    prompt("ship it", { at: 9_000, timeline_id: "p2" }),
    agent("Pushed.", { at: 10_000, model: "claude-opus-5", responseId: "r2" }),
  ];
  const turns = documentTurns(rows);

  it("cuts a turn at each prompt and nowhere else", () => {
    assert.equal(turns.length, 2);
    assert.equal(turns[0].prompt.text, "fix the fillet first, then the shadow");
    assert.equal(turns[1].prompt.text, "ship it");
    assert.equal(turns[0].key, "p1");
  });

  it("gives the reply ONE head with its model, effort, tokens and span", () => {
    const { head } = turns[0];
    assert.equal(head.model, "claude-opus-5");
    assert.equal(head.effort, "max");
    assert.equal(head.tokens, 41_200); // input + output + thinking, once per response
    assert.equal(head.startedAt, 2_000);
    assert.equal(head.endedAt, 8_000);
    assert.equal(head.spanMs, 6_000);
  });

  it("cuts the reply into runs at speech (R-1) and folds each run into ONE ledger", () => {
    assert.deepEqual(
      turns[0].segments.map((segment) => segment.kind),
      ["prose", "ledger", "prose", "ledger"],
    );
    const ledger = turns[0].segments[1];
    assert.equal(ledger.rows.length, 3); // the thought and the two commands
    assert.equal(ledger.lines.length, 1); // two commands fold to one line
    assert.deepEqual(ledger.members.map((member) => member.kind), ["thinking", "line"]);
    assert.equal(objectText(ledger.lines[0].object), "2 commands · pnpm typecheck, pnpm test");
    assert.deepEqual(ledger.summary.nouns.map((noun) => noun.text), ["2 commands"]);
    assert.equal(ledger.summary.closedByDefault, true);
    assert.equal(ledger.at, 3_000);
  });

  it("keeps every row reachable for anchors, focus landing and the watermark", () => {
    assert.equal(turns[0].rows.length, 8); // the prompt and everything that answered it
    assert.deepEqual(
      turns[0].rows.map((row) => row.ordinal),
      [1, 2, 3, 4, 5, 6, 7, 8],
    );
    const ledger = turns[0].segments[1];
    assert.ok(ledger.lines[0].rows.every((row) => typeof row.timeline_id === "string"));
  });

  it("takes a reasoning member's span from the adjacent stamps", () => {
    const reasoning = turns[0].segments[1].members[0];
    assert.equal(reasoning.kind, "thinking");
    assert.equal(reasoning.spanMs, 1_000);
  });

  it("draws a Notice as a hairline marker, red only for a runtime error", () => {
    const marker = turns[0].segments[3].members[0];
    assert.equal(marker.kind, "marker");
    assert.equal(marker.text, "context compacted");
    assert.equal(marker.tone, "plain");

    const [errorTurn] = documentTurns([
      prompt("go"),
      notice({ type: "runtime_error", kind: "provider", summary: "API 500 · at capacity" }),
    ]);
    const error = errorTurn.segments[0];
    assert.equal(error.tone, "failed");
    assert.equal(error.text, "runtime error · provider · API 500 · at capacity");
  });

  it("surfaces a delegate's proven child for the component to resolve", () => {
    const ledger = turns[0].segments[3];
    assert.deepEqual(ledger.lines[0].childSessionIds, ["child-1"]);
    assert.equal(ledger.lines[0].detail.kind, "delegate");
  });

  it("puts the eyebrow at the first agent-authored segment", () => {
    assert.equal(turns[0].headIndex, 0);
    const [late] = documentTurns([
      prompt("go"),
      notice({ type: "model_changed", from: null, to: "claude-opus-5" }),
      agent("done"),
    ]);
    assert.equal(late.segments[0].kind, "marker");
    assert.equal(late.headIndex, 1);
  });
});

/* ── group kinds ─────────────────────────────────────────────────────────── */

describe("group kinds decide what opens a turn", () => {
  it("agent_cycle, system_cycle and synthetic never open a turn", () => {
    const turns = documentTurns([
      prompt("go"),
      agent("thinking about it", { kind: "agent_cycle" }),
      notice({ type: "compaction", reason: null }, { kind: "system_cycle" }),
      shell("ls", { kind: "synthetic" }),
      summary("what happened", { kind: "synthetic" }),
    ]);
    assert.equal(turns.length, 1);
    // The notice, the command and the summary are one run between two things
    // said — the prose before them and the end of the reply.
    assert.deepEqual(
      turns[0].segments.map((segment) => segment.kind),
      ["prose", "ledger"],
    );
    assert.deepEqual(
      turns[0].segments[1].members.map((member) => member.kind),
      ["marker", "line", "summary"],
    );
  });

  it("parent_invocation opens a turn, flagged as the parent's words", () => {
    const turns = documentTurns([
      entry({ type: "user", text: inline("Report every consumer"), attachments: [] }, {
        kind: "parent_invocation",
      }),
      agent("On it."),
    ]);
    assert.equal(turns.length, 1);
    assert.equal(turns[0].prompt.fromParent, true);
  });

  it("rows before the first prompt become a headless opening turn", () => {
    const turns = documentTurns([agent("mid-answer, page two"), prompt("next"), agent("ok")]);
    assert.equal(turns.length, 2);
    assert.equal(turns[0].prompt, null);
    assert.equal(turns[0].segments.length, 1);
    assert.ok(turns[0].head !== null);
  });

  it("a turn nothing answered has no head at all", () => {
    const [turn] = documentTurns([prompt("hello?")]);
    assert.equal(turn.head, null);
    assert.deepEqual(turn.segments, []);
  });

  it("a user body opens a turn whatever its group claims", () => {
    // `user` means human-authored prose by contract, so a person speaking is
    // the document's structure even when the group derivation is synthetic.
    const turns = documentTurns([prompt("first"), prompt("second", { kind: "synthetic" })]);
    assert.equal(turns.length, 2);
  });
});

/* ── the command pill ────────────────────────────────────────────────────── */

describe("§03 H — the command pill and its output are one object", () => {
  it("absorbs the command_output notice that rides beside the pill", () => {
    const [turn] = documentTurns([
      prompt("/effort max", { at: 1_000 }),
      notice({ type: "command_output", output: inline("max") }, {
        at: 1_000,
      }),
      agent("ok"),
    ]);
    assert.equal(turn.prompt.isCommand, true);
    assert.equal(turn.prompt.command, "/effort max");
    assert.equal(turn.prompt.commandOutput, "max");
    assert.deepEqual(turn.prompt.setting, { kind: "effort", label: "Effort", value: "max" });
    assert.equal(turn.prompt.rows.length, 2);
    // The notice never also draws as a marker.
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["prose"]);
  });

  it("projects a normalized model value as a compact setting", () => {
    const [turn] = documentTurns([
      prompt("/model"),
      notice({ type: "command_output", output: inline("Opus 5 (1M context)") }),
    ]);
    assert.deepEqual(turn.prompt.setting, {
      kind: "model",
      label: "Model",
      value: "Opus 5 (1M context)",
    });
  });

  it("recognises a rejected command the same way", () => {
    const [turn] = documentTurns([
      prompt("/effort maxc"),
      notice({
        type: "command_output",
        output: inline("Invalid argument: maxc. Valid options are: low, medium, high"),
      }),
    ]);
    assert.equal(turn.prompt.isCommand, true);
    assert.ok(turn.prompt.commandOutput.startsWith("Invalid argument"));
    assert.equal(turn.prompt.setting, null);
  });

  it("leaves ordinary prose a panel, and its notice a marker", () => {
    const [turn] = documentTurns([
      prompt("run the whole suite before you call it done"),
      notice({ type: "command_output", output: inline("stray output") }),
    ]);
    assert.equal(turn.prompt.isCommand, false);
    assert.equal(turn.prompt.commandOutput, null);
    assert.equal(turn.segments[0].kind, "marker");
  });

  it("a command with no output still wears the pill", () => {
    const [turn] = documentTurns([prompt("/compact")]);
    assert.equal(turn.prompt.isCommand, true);
    assert.equal(turn.prompt.commandOutput, null);
  });
});

/* ── §06, the missing image ──────────────────────────────────────────────── */

describe("§06A — canonical image occurrences", () => {
  const ref = (id, source = { type: "unavailable", reason: "not_recorded" }, ranges = []) => ({
    id, kind: "image", label: null, name: `${id}.png`, media_type: "image/png",
    byte_len: null, dimensions: { width: 2048, height: 1536 }, source, message_ranges: ranges,
  });

  it("does not infer images or remove words from prose that looks like native markup", () => {
    for (const text of ["[Image: source: /tmp/3.png]", "[Image #4]", "<image>example</image>"]) {
      assert.deepEqual(readAttachments(text, []), { text, attachments: [] });
    }
  });

  it("preserves unavailable slots, identity, dimensions and Unicode-scalar ranges", () => {
    const text = "😀 [Image #4] compare [Image #5]";
    const refs = [ref("missing", undefined, [{ start: 2, end: 12 }]), ref("recorded", {
      type: "asset", asset: { url: "/assets/a.png", digest: "a", media_type: "image/png", byte_len: 1024 },
    }, [{ start: 21, end: 31 }])];
    const { attachments } = readAttachments(text, refs);
    assert.equal(attachments[0].missing, true);
    assert.equal(attachments[1].missing, false);
    assert.equal(attachments[0].key, "missing");
    assert.equal(attachments[0].caption, "2048×1536 · PNG");
    const mentions = attachmentMentions(text, attachments);
    assert.equal(mentions[0].target, attachments[0]);
    assert.equal(mentions[0].start, 3);
    assert.equal(text.slice(mentions[0].start, mentions[0].end), "[Image #4]");
    assert.equal(mentions[1].target, attachments[1]);
  });

  it("does not guess local or global numbering or let overlapping spans select an image", () => {
    const text = "[Image #4] and [Image #5]";
    assert.deepEqual(attachmentMentions(text, readAttachments(text, [ref("one"), ref("two")]).attachments), []);
    const conflicting = [ref("one", undefined, [{ start: 0, end: 10 }]), ref("two", undefined, [{ start: 2, end: 9 }])];
    assert.deepEqual(attachmentMentions(text, readAttachments(text, conflicting).attachments), []);
    const invalid = [ref("bad", undefined, [{ start: -1, end: 10 }, { start: 0, end: 999 }])];
    assert.deepEqual(attachmentMentions(text, readAttachments(text, invalid).attachments), []);
  });

  it("a file reference is not a claim that historical image bytes were stored", () => {
    const { attachments } = readAttachments(null, [ref("local", { type: "reference", reference: { type: "file", path: "/w/shot.png", resolved: true } })]);
    assert.equal(attachments[0].missing, true);
  });

  it("a short slash prompt with an image keeps its attachment panel", () => {
    const [turn] = documentTurns([prompt("/review", { attachments: [ref("input")] })]);
    assert.equal(turn.prompt.isCommand, false);
    assert.equal(turn.prompt.attachments.length, 1);
  });
});

/* ── §07's compact setting group, as a flag on the turn ─────────────────── */

describe("§07 — only adjacent human setting commands group", () => {
  it("groups successful settings inside five minutes with no reply between", () => {
    const turns = documentTurns([
      prompt("/model", { at: 1_000 }),
      notice({ type: "command_output", output: inline("Opus 5 (1M context)") }, { at: 1_000 }),
      prompt("/effort max", { at: 1_000 + 60_000 }),
      notice({ type: "command_output", output: inline("max") }, { at: 1_000 + 60_000 }),
    ]);
    assert.equal(turns[0].clusteredWithPrevious, false);
    assert.equal(turns[1].clusteredWithPrevious, true);
  });

  it("keeps every conversational user prompt standalone", () => {
    const turns = documentTurns([
      prompt("look at this", { at: 1_000 }),
      prompt("and the one from the ipad?", { at: 1_000 + 60_000 }),
    ]);
    assert.equal(turns[0].clusteredWithPrevious, false);
    assert.equal(turns[1].clusteredWithPrevious, false);
  });

  it("keeps a parent-agent prompt standalone, even when it resembles a setting", () => {
    const turns = documentTurns([
      prompt("/model", { at: 1_000 }),
      notice({ type: "command_output", output: inline("Opus 5") }, { at: 1_000 }),
      prompt("/effort max", { at: 2_000, kind: "parent_invocation" }),
      notice({ type: "command_output", output: inline("max") }, { at: 2_000 }),
    ]);
    assert.equal(turns[1].prompt.fromParent, true);
    assert.equal(turns[1].clusteredWithPrevious, false);
  });

  it("does not group settings once a reply stood between them", () => {
    const turns = documentTurns([
      prompt("/model", { at: 1_000 }),
      notice({ type: "command_output", output: inline("Opus 5") }, { at: 1_000 }),
      agent("Model selected.", { at: 2_000 }),
      prompt("/effort max", { at: 3_000 }),
      notice({ type: "command_output", output: inline("max") }, { at: 3_000 }),
    ]);
    assert.equal(turns[1].clusteredWithPrevious, false);
  });

  it("does not group settings across more than five minutes", () => {
    const turns = documentTurns([
      prompt("/model", { at: 1_000 }),
      notice({ type: "command_output", output: inline("Opus 5") }, { at: 1_000 }),
      prompt("/effort max", { at: 1_000 + 6 * 60_000 }),
      notice({ type: "command_output", output: inline("max") }, { at: 1_000 + 6 * 60_000 }),
    ]);
    assert.equal(turns[1].clusteredWithPrevious, false);
  });

  it("never groups an unstamped setting", () => {
    const turns = documentTurns([
      prompt("/model", { at: null }),
      notice({ type: "command_output", output: inline("Opus 5") }, { at: null }),
      prompt("/effort max", { at: 1_000 }),
      notice({ type: "command_output", output: inline("max") }, { at: 1_000 }),
    ]);
    assert.equal(turns[1].clusteredWithPrevious, false);
  });
});

/* ── runs — R-1 and R-2 (transcript-fold, 2026-09-08) ───────────────────── */

describe("documentTurns — the run", () => {
  it("a run of one line closes to its summary between prose", () => {
    const [turn] = documentTurns([prompt("go"), agent("Let me look."), shell("ls"), agent("Done.")]);
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["prose", "ledger", "prose"]);
    assert.equal(turn.segments[1].members.length, 1);
    assert.equal(turn.segments[1].summary.closedByDefault, true);
    assert.deepEqual(turn.segments[1].summary.nouns.map((noun) => noun.text), ["1 command"]);
  });

  it("a thought alone stays a thinking segment; two thoughts make a run", () => {
    const [alone] = documentTurns([prompt("go"), agent("A."), thinking("hm"), agent("B.")]);
    assert.deepEqual(alone.segments.map((segment) => segment.kind), ["prose", "thinking", "prose"]);
    const [pair] = documentTurns([prompt("go"), agent("A."), thinking("hm"), thinking("hmm"), agent("B.")]);
    assert.deepEqual(pair.segments.map((segment) => segment.kind), ["prose", "ledger", "prose"]);
    assert.deepEqual(pair.segments[1].summary.nouns.map((noun) => noun.text), ["2 thoughts"]);
  });

  it("notices alone never fold: they stand as their own markers", () => {
    const [turn] = documentTurns([
      prompt("go"),
      agent("A."),
      notice({ type: "compaction", reason: null }),
      notice({ type: "model_changed", from: null, to: "claude-opus-5" }),
      agent("B."),
    ]);
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["prose", "marker", "marker", "prose"]);
  });

  it("a blank agent row said nothing, so it closes nothing", () => {
    const [turn] = documentTurns([prompt("go"), shell("ls"), agent("   "), shell("cat"), agent("Done.")]);
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["ledger", "prose"]);
    assert.equal(turn.segments[0].lines.length, 1); // the two commands fold across it
  });

  it("a sitting boundary closes a run as speech does", () => {
    const rows = [prompt("go"), shell("ls", { timeline_id: "a" }), shell("cat", { timeline_id: "b" }), agent("Done.")];
    const [turn] = documentTurns(rows, { cutBefore: new Set(["b"]) });
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["ledger", "ledger", "prose"]);
    assert.equal(turn.segments[0].rows[0].timeline_id, "a");
    assert.equal(turn.segments[1].rows[0].timeline_id, "b");
  });

  it("a question to the reader is speech: it stands alone, open (R-1)", () => {
    const question = entry({
      type: "tool_use",
      tool: {
        correlation: { key: "q1", derivation: "native_id" },
        native: { id: null, namespace: null, name: "AskUserQuestion", provider: null },
        status: { value: "succeeded", basis: "native_state" },
        action: {
          type: "question",
          questions: [{ prompt: inline("Which theme?"), header: null, options: [], multi_select: false }],
          answers: [],
        },
        failure: null,
        timing: null,
      },
    });
    const [turn] = documentTurns([prompt("go"), shell("ls"), question, shell("cat"), agent("Done.")]);
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["ledger", "ledger", "ledger", "prose"]);
    assert.equal(turn.segments[1].lines[0].family, "question");
    assert.equal(turn.segments[1].summary.closedByDefault, false);
    assert.equal(turn.segments[0].lines[0].family, "shell");
    assert.equal(turn.segments[2].lines[0].family, "shell");
    assert.equal(turn.segments[0].summary.closedByDefault, true);
    assert.equal(turn.segments[2].summary.closedByDefault, true);
  });

  it("a thought between two commands is inside the run and breaks the line fold", () => {
    const [turn] = documentTurns([prompt("go"), shell("ls"), thinking("hm"), shell("cat"), agent("Done.")]);
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["ledger", "prose"]);
    const ledger = turn.segments[0];
    assert.deepEqual(ledger.members.map((member) => member.kind), ["line", "thinking", "line"]);
    assert.equal(ledger.lines.length, 2);
    assert.equal(ledger.summary.memberCount, 3);
    assert.deepEqual(ledger.summary.nouns.map((noun) => noun.text), ["2 commands"]);
  });
});

/* ── emptiness ───────────────────────────────────────────────────────────── */

describe("documentTurns is honest about nothing", () => {
  it("returns no turns for no rows", () => {
    assert.deepEqual(documentTurns([]), []);
  });

  it("drops empty prose and empty reasoning rather than drawing a blank", () => {
    const [turn] = documentTurns([prompt("go"), agent("   "), thinking("  \n "), agent("real")]);
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["prose"]);
    assert.equal(turn.segments[0].text, "real");
  });

  it("keeps an attachment-only prose row, which is not a blank", () => {
    const [turn] = documentTurns([
      prompt("go"),
      entry({
        type: "agent",
        text: null,
        attachments: [
          {
            id: "report", label: null, message_ranges: [],
            kind: "file",
            name: "report.pdf",
            media_type: "application/pdf",
            byte_len: 10,
            source: { type: "reference", reference: { type: "file", path: "/w/report.pdf", resolved: true } },
            dimensions: null,
          },
        ],
        citations: [],
      }),
    ]);
    assert.equal(turn.segments.length, 1);
    assert.equal(turn.segments[0].attachments[0].name, "report.pdf");
  });

  it("reports null tokens when the agent never reported any", () => {
    const [turn] = documentTurns([prompt("go"), agent("done")]);
    assert.equal(turn.head.tokens, null);
  });
});
