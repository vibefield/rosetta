import { strict as assert } from "node:assert";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import {
  createTreeSizes, measureWithDu,
  TREE_SIZE_ASK_MAX, TREE_SIZE_FAILED_TTL_MS, TREE_SIZE_REST_FACTOR, TREE_SIZE_REST_MIN_MS, TREE_SIZE_TTL_MS,
} from "../electron/tree-size.mjs";

const settle = async () => { for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0)); };

/** A measurer and a rest the test releases by hand: who was walked, in order, and each pause asked for. */
function gated() {
  const walked = [], rests = [];
  const release = new Map();
  let wake = null;
  return {
    walked, rests,
    measure: (folder) => new Promise((resolve) => { walked.push(folder); release.set(folder, resolve); }),
    rest: (ms) => new Promise((resolve) => { rests.push(ms); wake = resolve; }),
    answer: async (folder, bytes) => { release.get(folder)(bytes); await settle(); },
    rested: async () => { wake?.(); wake = null; await settle(); },
  };
}
const over = (gate, more = {}) =>
  createTreeSizes({ measure: gate.measure, rest: gate.rest, isWorktree: () => true, platform: "darwin", ...more });

describe("tree sizes", () => {
  it("walks nothing until asked; then one folder at a time, resting after each", async () => {
    const gate = gated();
    const sizes = over(gate);
    assert.equal(sizes.peek("/a"), undefined);
    await settle();
    assert.deepEqual(gate.walked, [], "a peek queues nothing");
    assert.deepEqual(sizes.ask(["/a", "/b"]), [null, null]);
    await settle();
    assert.deepEqual(gate.walked, ["/a"], "one walk at a time");
    await gate.answer("/a", 4096);
    assert.deepEqual(sizes.ask(["/a", "/b"]), [4096, null], "the figure is answerable at once");
    assert.deepEqual(gate.walked, ["/a"], "the next walk waits out the rest");
    assert.deepEqual(gate.rests, [TREE_SIZE_REST_MIN_MS]);
    await gate.rested();
    assert.deepEqual(gate.walked, ["/a", "/b"]);
    await gate.answer("/b", 8192);
    assert.deepEqual(sizes.ask(["/a", "/b"]), [4096, 8192]);
  });

  it("rests in proportion to the walk it just made", async () => {
    const gate = gated();
    let clock = 10_000;
    const sizes = over(gate, { now: () => clock });
    sizes.ask(["/slow"]);
    await settle();
    clock += 3_000;
    await gate.answer("/slow", 1);
    assert.deepEqual(gate.rests, [3_000 * TREE_SIZE_REST_FACTOR]);
  });

  it("an ask replaces the queue: what is no longer named is no longer owed", async () => {
    const gate = gated();
    const sizes = over(gate);
    sizes.ask(["/a", "/b", "/c", "/d"]);
    await settle();
    assert.deepEqual(sizes.ask(["/d", "/e"]), [null, null]);
    await gate.answer("/a", 1); await gate.rested();
    await gate.answer("/d", 2); await gate.rested();
    await gate.answer("/e", 3); await gate.rested();
    assert.deepEqual(gate.walked, ["/a", "/d", "/e"], "/b and /c were never walked");
    assert.equal(sizes.peek("/b"), undefined);
  });

  it("takes at most the ask's limit of folders", () => {
    const answers = over(gated()).ask(Array.from({ length: TREE_SIZE_ASK_MAX + 5 }, (_, i) => `/w${i}`));
    assert.ok(answers.slice(0, TREE_SIZE_ASK_MAX).every((a) => a === null));
    assert.ok(answers.slice(TREE_SIZE_ASK_MAX).every((a) => a === undefined));
  });

  it("a figure stands for a day; after it, the old figure is answered while it is walked again", async () => {
    const gate = gated();
    let clock = 1_000;
    const sizes = over(gate, { now: () => clock });
    sizes.ask(["/a"]);
    await settle();
    await gate.answer("/a", 100); await gate.rested();
    clock += TREE_SIZE_TTL_MS - 1;
    assert.deepEqual(sizes.ask(["/a"]), [100]);
    await settle();
    assert.equal(gate.walked.length, 1);
    clock += 1;
    assert.deepEqual(sizes.ask(["/a"]), [100], "the old figure stands while the new walk runs");
    await settle();
    assert.equal(gate.walked.length, 2);
    await gate.answer("/a", 250);
    assert.deepEqual(sizes.ask(["/a"]), [250]);
  });

  it("remembers a failed walk as no figure, for its shorter while", async () => {
    const gate = gated();
    let clock = 1_000;
    const sizes = over(gate, { now: () => clock });
    sizes.ask(["/a"]);
    await settle();
    await gate.answer("/a", null); await gate.rested();
    assert.deepEqual(sizes.ask(["/a"]), [undefined]);
    await settle();
    assert.equal(gate.walked.length, 1);
    clock += TREE_SIZE_FAILED_TTL_MS;
    assert.deepEqual(sizes.ask(["/a"]), [null]);
  });

  it("keeps its figures on disk: a new measurer over the same file walks nothing", async (t) => {
    const dir = mkdtempSync(path.join(tmpdir(), "rosetta-tree-kept-"));
    t.after(() => rmSync(dir, { recursive: true, force: true }));
    const file = path.join(dir, "state", "worktree-sizes.json");
    const gate = gated();
    const first = over(gate, { file });
    first.ask(["/a", "/b"]);
    await settle();
    await gate.answer("/a", 4096); await gate.rested();
    await gate.answer("/b", null);
    first.dispose();
    assert.deepEqual(Object.keys(JSON.parse(readFileSync(file, "utf8")).sizes), ["/a"], "a failed walk is not kept");
    const again = gated();
    const second = over(again, { file });
    assert.equal(second.peek("/a"), 4096);
    assert.deepEqual(second.ask(["/a"]), [4096]);
    await settle();
    assert.deepEqual(again.walked, []);
    writeFileSync(file, "{ not json");
    assert.equal(over(gated(), { file }).peek("/a"), undefined, "an unreadable file is no figures");
  });

  it("never walks a folder that is not a checkout", async () => {
    const gate = gated();
    const sizes = createTreeSizes({ measure: gate.measure, rest: async () => {}, platform: "darwin" });
    assert.deepEqual(sizes.ask(["/", "relative/path"]), [null, null]);
    for (let i = 0; i < 6; i++) await settle();
    assert.deepEqual(gate.walked, []);
    assert.deepEqual(sizes.ask(["/", "relative/path"]), [undefined, undefined]);
  });

  it("measures nothing on Windows, and nothing once disposed", async () => {
    const gate = gated();
    assert.deepEqual(over(gate, { platform: "win32" }).ask(["C:\\repo"]), [undefined]);
    const sizes = over(gate);
    sizes.dispose();
    assert.deepEqual(sizes.ask(["/a"]), [undefined]);
    await settle();
    assert.deepEqual(gate.walked, []);
  });

  it("reads a real folder with du, in the background where the platform has one", { skip: process.platform === "win32" }, async (t) => {
    const dir = mkdtempSync(path.join(tmpdir(), "rosetta-tree-size-"));
    t.after(() => rmSync(dir, { recursive: true, force: true }));
    mkdirSync(path.join(dir, ".git"));
    mkdirSync(path.join(dir, "deep", "er"), { recursive: true });
    writeFileSync(path.join(dir, "deep", "er", "blob"), Buffer.alloc(300_000, 7));
    const bytes = await measureWithDu(dir);
    assert.ok(bytes >= 300_000 && bytes < 2_000_000, `du said ${bytes}`);
    assert.equal(await measureWithDu(dir, { background: false }), bytes);
    assert.equal(await measureWithDu(path.join(dir, "absent")), null);
    const sizes = createTreeSizes();
    t.after(() => sizes.dispose());
    assert.deepEqual(sizes.ask([dir]), [null]);
    for (let i = 0; i < 400 && sizes.peek(dir) === undefined; i++) await new Promise((resolve) => setTimeout(resolve, 10));
    assert.equal(sizes.peek(dir), bytes);
  });
});
