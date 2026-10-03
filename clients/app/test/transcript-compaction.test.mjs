import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import fs from "node:fs";
import ts from "typescript";
import * as transcript from "../../room/src/lib/transcript/index.ts";
import { documentPlan } from "../../room/src/components/transcript/document-plan.ts";
import { entry, inline, shell } from "./support/ledger-fixtures.mjs";

// Load the production model with its normal pure dependencies; only React's
// unused memo hook is stubbed for this Node test.
const source = fs.readFileSync(new URL("../../room/src/components/transcript/document-model.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } });
const exports = {};
new Function("require", "exports", outputText)((name) => {
  if (name === "react") return {};
  if (name === "../../lib/transcript") return transcript;
  if (name === "./document-plan") return { documentPlan };
  throw new Error(`Unexpected dependency: ${name}`);
}, exports);
const { transcriptModel } = exports;
const { documentTurns } = transcript;

function pair() {
  const boundary = { ...entry({ type: "notice", notice: { type: "compaction", reason: null } }),
    ordinal: 2, native_id: "boundary", at: 2_500 };
  const summary = { ...entry({ type: "summary", purpose: "compaction", text: inline("# Recap\n\nPreserved context.") }),
    ordinal: 3, native_id: "recap", parent_native_id: "boundary", at: 2_100 };
  return [boundary, summary];
}
const agent = () => entry({ type: "agent", text: inline("The reply."), attachments: [], citations: [] });

describe("compaction context", () => {
  it("joins an exact boundary and recap without a human turn or response head", () => {
    const rows = pair();
    const [turn] = documentTurns(rows);
    assert.equal(turn.prompt, null);
    assert.equal(turn.head, null);
    assert.equal(turn.segments.length, 1);
    assert.equal(turn.segments[0].kind, "compaction");
    assert.deepEqual(turn.segments[0].rows, rows);
    assert.equal(turn.segments[0].row, rows[1]);
    assert.deepEqual(turn.rows, rows);
  });

  it("keeps both search anchors and the loaded ordinals on one disclosure", () => {
    const rows = pair();
    const model = transcriptModel(rows, null, null, 3_000);
    assert.equal(model.representatives.get(rows[0].timeline_id), rows[1].timeline_id);
    assert.equal(model.representatives.get(rows[1].timeline_id), rows[1].timeline_id);
    assert.equal(model.gaps.size, 0);
    assert.equal(model.plan.length, 1);
    assert.equal(model.plan[0].parts[0].firstRowId, rows[1].timeline_id);
  });

  it("keeps recaps outside activity folds, with the head above actual agent speech", () => {
    const [leading] = documentTurns([...pair(), agent()]);
    assert.equal(leading.headIndex, 1);
    const [turn] = documentTurns([shell("ls"), ...pair(), shell("cat README")]);
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["ledger", "compaction", "ledger"]);
  });

  it("draws a standalone recap when its boundary has not been loaded", () => {
    const [turn] = documentTurns([pair()[1]]);
    assert.equal(turn.head, null);
    assert.equal(turn.prompt, null);
    assert.equal(turn.segments[0].kind, "compaction");
    assert.equal(turn.segments[0].rows.length, 1);
  });

  it("does not merge unrelated, cross-session, nonadjacent, or split-sitting rows", () => {
    for (const alter of [
      (rows) => { rows[1].parent_native_id = "different"; },
      (rows) => { rows[1].session_id = "other"; },
      (rows) => { rows[0].native_id = null; },
      (rows) => { rows[1].ordinal += 2; },
      (rows) => { rows.splice(1, 0, agent()); },
    ]) {
      const rows = pair();
      alter(rows);
      const [turn] = documentTurns(rows);
      assert.equal(turn.segments.filter((segment) => segment.kind === "marker").length, 1);
      assert.equal(turn.segments.find((segment) => segment.kind === "compaction").rows.length, 1);
    }
    const rows = pair();
    const [turn] = documentTurns(rows, { cutBefore: new Set([rows[1].timeline_id]) });
    assert.deepEqual(turn.segments.map((segment) => segment.kind), ["marker", "compaction"]);
  });

  it("supports reversed source ordering only when the native relation still matches", () => {
    const rows = pair().reverse().map((row, index) => ({ ...row, ordinal: index + 1 }));
    const [turn] = documentTurns(rows);
    assert.equal(turn.segments.length, 1);
    assert.deepEqual(turn.segments[0].rows, rows);
  });

  it("keeps the real /compact command and its output as a command pill", () => {
    const command = entry({ type: "user", text: inline("/compact"), attachments: [] });
    const result = entry({ type: "notice", notice: { type: "command_output", output: inline("Compacted") } });
    const turns = documentTurns([...pair(), command, result]);
    assert.equal(turns.length, 2);
    assert.equal(turns[0].prompt, null);
    assert.equal(turns[1].prompt.command, "/compact");
    assert.equal(turns[1].prompt.commandOutput, "Compacted");
    assert.equal(turns[1].head, null);
  });

  it("preserves the full member reference instead of flattening it to a preview", () => {
    const summary = pair()[1];
    summary.body.text = { type: "member", preview: "Only a preview", asset: { digest: "abc", url: "/assets/abc" } };
    const [turn] = documentTurns([summary]);
    assert.equal(turn.segments[0].text, summary.body.text);
  });
});
