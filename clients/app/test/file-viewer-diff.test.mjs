import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { parseDiff } from "../../room/src/components/file-viewer/diff/parse.ts";

describe("file viewer diff parser", () => {
  it("renders Codex apply_patch hunks without inventing line numbers", () => {
    const parsed = parseDiff(
      "--- src/main.rs\n+++ src/main.rs\n@@\n context\n-old\n+new",
    );
    assert.equal(parsed.hunks.length, 1);
    assert.equal(parsed.hunks[0].oldStart, null);
    assert.equal(parsed.hunks[0].newStart, null);
    assert.deepEqual(
      parsed.hunks[0].lines.map((line) => [line.kind, line.oldNumber, line.newNumber]),
      [
        ["context", null, null],
        ["del", null, null],
        ["add", null, null],
      ],
    );
    assert.equal(parsed.added, 1);
    assert.equal(parsed.deleted, 1);
  });

  it("keeps native unified-diff coordinates", () => {
    const parsed = parseDiff(
      "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -12,2 +12,2 @@ main\n old\n-new\n+newer",
    );
    assert.equal(parsed.hunks[0].oldStart, 12);
    assert.equal(parsed.hunks[0].newStart, 12);
    assert.deepEqual(
      parsed.hunks[0].lines.map((line) => [line.oldNumber, line.newNumber]),
      [
        [12, 12],
        [13, null],
        [null, 13],
      ],
    );
  });

  it("does not read the text's final newline as a line of the last hunk", () => {
    const hunk = "--- a/notes.md\n+++ b/notes.md\n@@ -4,3 +4,3 @@\n kept\n-old\n+new\n";
    const lines = (text) => parseDiff(text).hunks.at(-1).lines.map((line) => [line.kind, line.oldNumber, line.newNumber]);
    const expected = [
      ["context", 4, 4],
      ["del", 5, null],
      ["add", null, 5],
    ];
    assert.deepEqual(lines(hunk), expected);
    // A blank context line whose space git dropped is still a line, before the end.
    assert.deepEqual(lines(`${hunk}\n`), [...expected, ["context", 6, 6]]);
    assert.deepEqual(lines(`${hunk}\n kept too\n`), [...expected, ["context", 6, 6], ["context", 7, 7]]);
  });
});
