import { strict as assert } from "node:assert";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { createTreeSizes, measureWithDu } from "../electron/tree-size.mjs";

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A measurer the test releases by hand: who was asked, in order, and when. */
function gated() {
  const asked = [];
  const release = new Map();
  return {
    asked,
    measure: (folder) => new Promise((resolve) => { asked.push(folder); release.set(folder, resolve); }),
    answer: async (folder, bytes) => { release.get(folder)(bytes); await settle(); await settle(); },
  };
}

describe("tree sizes", () => {
  it("answers at once, measures one folder at a time, and has the figure for the next read", async () => {
    const gate = gated();
    const sizes = createTreeSizes({ measure: gate.measure, isWorktree: () => true, platform: "darwin" });
    assert.equal(sizes.read("/a"), null);
    assert.equal(sizes.read("/b"), null);
    assert.equal(sizes.read("/a"), null, "a second read queues nothing more");
    await settle();
    assert.deepEqual(gate.asked, ["/a"], "one walk at a time");
    await gate.answer("/a", 4096);
    assert.deepEqual(gate.asked, ["/a", "/b"]);
    assert.equal(sizes.read("/a"), 4096);
    await gate.answer("/b", 8192);
    assert.equal(sizes.read("/b"), 8192);
    assert.deepEqual(gate.asked, ["/a", "/b"], "a standing figure is not measured again");
  });

  it("answers the old figure while a stale one is measured again", async () => {
    const gate = gated();
    let clock = 1_000;
    const sizes = createTreeSizes({ measure: gate.measure, isWorktree: () => true, platform: "linux", ttlMs: 500, now: () => clock });
    sizes.read("/a");
    await settle();
    await gate.answer("/a", 100);
    clock += 499;
    assert.equal(sizes.read("/a"), 100);
    assert.equal(gate.asked.length, 1);
    clock += 1;
    assert.equal(sizes.read("/a"), 100, "the old figure stands while the new walk runs");
    await settle();
    assert.equal(gate.asked.length, 2);
    await gate.answer("/a", 250);
    assert.equal(sizes.read("/a"), 250);
  });

  it("remembers a failed walk as no figure, and does not walk again until it is old", async () => {
    const gate = gated();
    const sizes = createTreeSizes({ measure: gate.measure, isWorktree: () => true, platform: "darwin" });
    sizes.read("/a");
    await settle();
    await gate.answer("/a", null);
    assert.equal(sizes.read("/a"), undefined);
    await settle();
    assert.equal(gate.asked.length, 1);
  });

  it("never walks a folder that is not a checkout", async () => {
    const gate = gated();
    const sizes = createTreeSizes({ measure: gate.measure, platform: "darwin" });
    assert.equal(sizes.read("/"), null);
    assert.equal(sizes.read("relative/path"), null);
    await settle(); await settle();
    assert.deepEqual(gate.asked, []);
    assert.equal(sizes.read("/"), undefined);
    assert.equal(sizes.read("relative/path"), undefined);
  });

  it("measures nothing on Windows, and nothing once disposed", async () => {
    const gate = gated();
    assert.equal(createTreeSizes({ measure: gate.measure, isWorktree: () => true, platform: "win32" }).read("C:\\repo"), undefined);
    const sizes = createTreeSizes({ measure: gate.measure, isWorktree: () => true, platform: "darwin" });
    sizes.dispose();
    assert.equal(sizes.read("/a"), undefined);
    await settle();
    assert.deepEqual(gate.asked, []);
  });

  it("reads a real folder with du", { skip: process.platform === "win32" }, async (t) => {
    const dir = mkdtempSync(path.join(tmpdir(), "rosetta-tree-size-"));
    t.after(() => rmSync(dir, { recursive: true, force: true }));
    mkdirSync(path.join(dir, ".git"));
    mkdirSync(path.join(dir, "deep", "er"), { recursive: true });
    writeFileSync(path.join(dir, "deep", "er", "blob"), Buffer.alloc(300_000, 7));
    const bytes = await measureWithDu(dir);
    assert.ok(bytes >= 300_000 && bytes < 2_000_000, `du said ${bytes}`);
    assert.equal(await measureWithDu(path.join(dir, "absent")), null);

    const sizes = createTreeSizes();
    assert.equal(sizes.read(dir), null);
    for (let i = 0; i < 200 && sizes.read(dir) === null; i++) await new Promise((resolve) => setTimeout(resolve, 10));
    assert.equal(sizes.read(dir), bytes);
  });
});
