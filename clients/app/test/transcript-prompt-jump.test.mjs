// A button at the side goes to the message before, as pure functions: which of the person's own messages a press
// goes to, where it lands, how it moves, and when the button is there at all. The DOM reads (the messages' tops, the
// head's edge) are the component's; what is decided from them is here, so a test names it.
//
// The names are read off the module whole, so a build without them fails its own rows, not the file.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import * as transcript from "../../room/src/lib/transcript/index.ts";

const exported = (name) => assert.equal(typeof transcript[name], "function", `${name} is exported`);

describe("a button at the side goes to the message before", () => {
  // Tops are in the scroller's own coordinates, from the top of its frame; the view's top is the head's edge.
  const viewTop = 28;

  it("goes to the nearest own message that begins above the top of the view", () => {
    exported("previousMessageTarget");
    assert.equal(transcript.previousMessageTarget([-900, -300, 50, 400], viewTop), 1);
    assert.equal(transcript.previousMessageTarget([-300], viewTop), 0);
  });

  it("a message whose top is in the view, or at its top, is not above it", () => {
    exported("previousMessageTarget");
    assert.equal(transcript.previousMessageTarget([viewTop, 50, 400], viewTop), null);
    assert.equal(transcript.previousMessageTarget([], viewTop), null);
  });

  it("pressed again it goes to the one before: a landed message stands 12 under the head, in the view", () => {
    exported("previousMessageTarget");
    exported("jumpScrollTop");
    // The target at -300 lands 12 under the head; the view's tops move with it.
    const landed = transcript.jumpScrollTop(2_000, -300, viewTop);
    assert.equal(landed, 2_000 - 300 - viewTop - 12);
    const shift = 2_000 - landed;
    const after = [-900, -300, 50].map((top) => top + shift);
    assert.equal(after[1], viewTop + 12);
    assert.equal(transcript.previousMessageTarget(after, viewTop), 0);
  });

  it("two messages at one top: the earlier of them", () => {
    exported("previousMessageTarget");
    assert.equal(transcript.previousMessageTarget([-500, -300, -300, 80], viewTop), 1);
  });

  it("the landing never asks for a scroll above the document's top", () => {
    exported("jumpScrollTop");
    assert.equal(transcript.jumpScrollTop(100, -500, viewTop), 0);
  });

  it("more than a screen away it goes at once; nearer on the 300 ms ease; never animated under Reduce Motion", () => {
    exported("jumpMotion");
    assert.equal(transcript.jumpMotion(1_200, 800, false), "instant");
    assert.equal(transcript.jumpMotion(-1_200, 800, false), "instant");
    assert.equal(transcript.jumpMotion(400, 800, false), "ease");
    assert.equal(transcript.jumpMotion(-400, 800, false), "ease");
    assert.equal(transcript.jumpMotion(400, 800, true), "instant");
    assert.equal(transcript.PROMPT_JUMP_EASE_MS, 300);
  });

  it("the ease is the room's settle curve, cubic-bezier(0.22, 1, 0.36, 1)", () => {
    exported("settleEase");
    assert.equal(transcript.settleEase(0), 0);
    assert.equal(transcript.settleEase(1), 1);
    for (const [x, y] of [[0.1, 0.4011], [0.25, 0.7649], [0.5, 0.9614], [0.75, 0.9969]]) {
      assert.ok(Math.abs(transcript.settleEase(x) - y) < 1e-3, `settleEase(${x}) ≈ ${y}`);
    }
    let previous = 0;
    for (let i = 1; i <= 100; i += 1) {
      const value = transcript.settleEase(i / 100);
      assert.ok(value >= previous, "the ease never turns back");
      previous = value;
    }
  });

  it("it is there while an own message begins above, or rows above are not yet read", () => {
    exported("promptJumpShown");
    assert.equal(transcript.promptJumpShown(true, "none"), true);
    assert.equal(transcript.promptJumpShown(false, "idle"), true);
    assert.equal(transcript.promptJumpShown(false, "fetching"), true);
    assert.equal(transcript.promptJumpShown(false, "none"), false);
  });

  it("at the first message: with rows above unread a press goes to the document's top; with none, nowhere", () => {
    exported("promptJumpPlan");
    const unread = transcript.promptJumpPlan({ tops: [120], viewTop, scrollTop: 60, earlier: "idle" });
    assert.deepEqual(unread, { index: null, scrollTop: 0 });
    assert.equal(
      transcript.promptJumpPlan({ tops: [120], viewTop, scrollTop: 60, earlier: "none" }),
      null,
    );
    const above = transcript.promptJumpPlan({ tops: [-300, 120], viewTop, scrollTop: 900, earlier: "idle" });
    assert.deepEqual(above, { index: 0, scrollTop: 900 - 300 - viewTop - 12 });
  });
});
