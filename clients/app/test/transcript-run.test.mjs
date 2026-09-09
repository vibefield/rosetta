// The run's closing sentence — the head every run of two or more members
// closes to (transcript-fold R-3, 2026-09-08). Fixtures are hand-built
// TimelineEntry objects: never a live home directory, never a transcript.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  FOLD_NAME_LIMIT,
  RUN_FOLD_THRESHOLD,
  ledgerLines,
  objectText,
  runSummary,
} from "../renderer/src/lib/transcript/index.ts";
import { CTX, entry, file, inline, read, shell, tool } from "./support/ledger-fixtures.mjs";

/* ── the run's closing sentence ──────────────────────────────────────────── */

describe("the run summary — the head of a closed run", () => {
  const change = (path = "/w/a.ts") =>
    tool({
      type: "file_change",
      changes: [
        {
          operation: "update",
          path: file(path),
          destination: null,
          diff: null,
          lines_added: 1,
          lines_removed: 1,
        },
      ],
    });
  const thought = () => ({ kind: "thinking", row: entry({ type: "thinking", text: inline("hm") }) });
  const marker = (failed) => ({
    kind: "marker",
    row: entry({ type: "notice", notice: { type: "compaction", reason: null } }),
    failed,
  });
  const text = (summary) => summary.nouns.map((noun) => noun.text);

  it("names NOUNS acts first, each class by count, and counts rows, not lines", () => {
    const rows = [
      read("/w/a.ts"),
      read("/w/b.ts"),
      change(),
      shell("ls"),
      shell("cat"),
      shell("grep"),
    ];
    const summary = runSummary(ledgerLines(rows, CTX));
    // The edit is the work product and leads (R-3); among the looks the three
    // commands outrank the two reads, whatever order they happened in.
    assert.deepEqual(text(summary), ["1 edit", "3 commands", "2 reads"]);
    assert.equal(summary.rowCount, 6);
    assert.equal(summary.lineCount, 3);
    assert.equal(summary.memberCount, 3);
  });

  it("names the changed files up to the fold limit, then counts the rest", () => {
    const rows = ["a", "b", "c", "d", "e", "a"].map((name) => change(`/w/${name}.ts`));
    const summary = runSummary(ledgerLines(rows, CTX));
    assert.equal(summary.nouns[0].family, "file_change");
    assert.equal(summary.nouns[0].text, "6 edits");
    assert.deepEqual(summary.nouns[0].names, ["a.ts", "b.ts", "c.ts", "d.ts"]);
    assert.equal(summary.nouns[0].more, 1);
    assert.equal(FOLD_NAME_LIMIT, 4);
  });

  it("counts failures separately and measures the whole run's span", () => {
    const rows = [
      shell("a", { at: 1_000 }),
      shell("b", {
        at: 5_000,
        status: "failed",
        failure: { kind: "unknown", summary: "boom", diagnostic: null },
      }),
      shell("c", { at: 10_000 }),
    ];
    const summary = runSummary(ledgerLines(rows, CTX));
    assert.equal(summary.failed, 1);
    assert.equal(summary.spanMs, 9_000);
    assert.deepEqual(text(summary), ["3 commands"]);
  });

  it(`closes at ${RUN_FOLD_THRESHOLD} members and leaves a run of one as the line (R-2)`, () => {
    const one = runSummary(ledgerLines([change()], CTX));
    assert.equal(one.memberCount, 1);
    assert.equal(one.closedByDefault, false);
    const two = runSummary(ledgerLines([change("/w/a.ts"), change("/w/b.ts")], CTX));
    assert.equal(two.memberCount, 2);
    assert.equal(two.closedByDefault, true);
  });

  it("a thought is a member of the run and silent in its sentence", () => {
    const summary = runSummary(ledgerLines([change()], CTX), [thought()]);
    assert.equal(summary.memberCount, 2);
    assert.equal(summary.thoughtCount, 1);
    assert.equal(summary.closedByDefault, true);
    assert.deepEqual(text(summary), ["1 edit"]);
  });

  it("a run of thoughts alone says the one thing it holds", () => {
    const summary = runSummary([], [thought(), thought()]);
    assert.deepEqual(text(summary), ["2 thoughts"]);
    assert.equal(summary.nouns[0].family, "thinking");
    assert.equal(summary.closedByDefault, true);
  });

  it("counts a failed notice inside the run as an error, beside the failures", () => {
    const summary = runSummary(ledgerLines([shell("ls"), shell("cat")], CTX), [
      marker(true),
      marker(false),
    ]);
    assert.equal(summary.errors, 1);
    assert.equal(summary.failed, 0);
    assert.equal(summary.memberCount, 3);
    assert.equal(summary.rowCount, 4);
  });

  it("a task run folds like any other — the island keeps the board (Q12 A)", () => {
    const task = tool({
      type: "task",
      operation: "create",
      task_id: "1",
      subject: "Audit the renderer",
      description: null,
      status: "pending",
      owner: null,
      blocker_ids: [],
      snapshot: {
        tasks: [
          {
            task_id: "1",
            subject: "Audit the renderer",
            status: "pending",
            owner: null,
            blocker_ids: [],
          },
        ],
        total_count: 1,
      },
    });
    const summary = runSummary(ledgerLines([task, change()], CTX));
    assert.equal(summary.closedByDefault, true);
    assert.deepEqual(text(summary), ["1 task", "1 edit"]);
  });

  it("surfaces the last running line for a live head (R-5)", () => {
    const rows = [shell("ls"), shell("pnpm test", { status: "running" })];
    const summary = runSummary(ledgerLines(rows, CTX));
    assert.equal(summary.running?.verb, "Ran");
    assert.equal(objectText(summary.running.object), "pnpm test");
    assert.equal(runSummary(ledgerLines([shell("ls")], CTX)).running, null);
  });

  it("an empty run says nothing", () => {
    const summary = runSummary([]);
    assert.deepEqual(summary.nouns, []);
    assert.equal(summary.spanMs, null);
    assert.equal(summary.closedByDefault, false);
  });
});

