import assert from "node:assert/strict";
import { test } from "node:test";
import { ListMotion } from "../../room/src/lib/motion.ts";

function list(t, count = 5) {
  const originalStyle = globalThis.getComputedStyle;
  const originalMatrix = globalThis.DOMMatrixReadOnly;
  globalThis.getComputedStyle = (element) => ({
    transform: String(element.displacement),
    opacity: String(element.opacity),
  });
  globalThis.DOMMatrixReadOnly = class {
    constructor(transform) {
      this.m42 = Number(transform);
    }
  };
  t.after(() => {
    globalThis.getComputedStyle = originalStyle;
    globalThis.DOMMatrixReadOnly = originalMatrix;
  });
  const animations = [];
  const container = {
    scrollTop: 0,
    clientHeight: 300,
    getBoundingClientRect: () => ({ top: 100 }),
    querySelectorAll: () => rows,
  };
  const rows = Array.from({ length: count }, (_, i) => ({
    dataset: { motionKey: String(i) },
    y: i * 60,
    displacement: 0,
    opacity: 1,
    getBoundingClientRect() {
      return { top: this.y + 100 - container.scrollTop, height: 60 };
    },
    animate(frames) {
      const animation = {
        frames,
        key: this.dataset.motionKey,
        cancelled: false,
        cancel() {
          this.cancelled = true;
        },
        onfinish: null,
      };
      animations.push(animation);
      return animation;
    },
  }));
  const motion = new ListMotion();
  t.after(() => motion.cancel());
  const reverse = () => {
    rows.reverse();
    rows.forEach((row, i) => {
      row.y = i * 60;
    });
  };
  return { motion, rows, container, animations, reverse };
}

test("mount is quiet, reordering animates from the old layout, and scrolling does not animate", (t) => {
  const s = list(t);
  s.motion.update(s.container, false);
  assert.equal(s.animations.length, 0);
  s.container.scrollTop = 100;
  s.motion.update(s.container, false);
  assert.equal(s.animations.length, 0);
  s.reverse();
  s.motion.update(s.container, false);
  assert.equal(s.animations[0].frames[0].transform, "translateY(240px)");
});

test("rapid reordering picks up the current visual offset instead of the old destination", (t) => {
  const s = list(t);
  s.motion.update(s.container, false);
  s.reverse();
  s.motion.update(s.container, false);
  const target = s.rows[0];
  target.displacement = 130; // halfway through the native animation
  s.reverse();
  s.motion.update(s.container, false);
  const retarget = s.animations.filter((animation) => animation.key === "4").at(-1);
  assert.equal(retarget.frames[0].transform, "translateY(-110px)");
  assert.equal(s.animations[0].cancelled, true);
});

test("large lists only animate near the viewport; reduced motion cancels all owned work", (t) => {
  const s = list(t, 2000);
  s.motion.update(s.container, false);
  s.reverse();
  s.motion.update(s.container, false);
  assert.ok(s.animations.length <= 12, "offscreen rows must not allocate animation layers");
  s.reverse();
  const before = s.animations.length;
  s.motion.update(s.container, true);
  assert.equal(s.animations.length, before);
  assert.ok(s.animations.every((animation) => animation.cancelled));
});

test("a newly added row enters without restarting its opacity on the next update", (t) => {
  const s = list(t, 2);
  s.motion.update(s.container, false);
  const original = s.rows[0];
  const added = { ...original, dataset: { motionKey: "new" }, y: 120 };
  s.rows.push(added);
  s.motion.update(s.container, false);
  assert.equal(s.animations.at(-1).frames[0].opacity, 0);
  added.opacity = 0.4;
  added.displacement = 3;
  s.motion.update(s.container, false);
  assert.equal(s.animations.at(-1).frames[0].opacity, 0.4);
});
