import assert from "node:assert/strict";
import { test } from "node:test";
import { TailArrivals, tailStep } from "../renderer/src/lib/transcript/tail-state.ts";
import { TailScroll } from "../renderer/src/lib/transcript/tail-scroll.ts";

const row = (ordinal, session_id = "root") => ({
  session_id,
  ordinal,
  timeline_id: `${session_id}-${ordinal}`,
});

test("only appended rows count: snapshots, prepends, patches and replay do not", () => {
  const arrivals = new TailArrivals();
  assert.deepEqual(arrivals.update([row(50), row(51)]), []);
  assert.deepEqual(arrivals.update([row(1), row(49), row(50), row(51)]), []);
  assert.deepEqual(arrivals.update([row(50), { ...row(51), text: "streamed text" }]), []);
  assert.deepEqual(arrivals.update([row(50), row(51), row(54), row(57)]), [row(54), row(57)]);
  assert.deepEqual(arrivals.update([row(51), row(54), row(57)]), []);
});

test("merged team chronology uses per-session baselines, including empty snapshots", () => {
  const arrivals = new TailArrivals();
  arrivals.update([row(100)]);
  assert.deepEqual(arrivals.update([row(4, "worker"), row(100)]), []);
  assert.deepEqual(arrivals.update([row(5, "worker"), row(100), row(101)]), [
    row(5, "worker"),
    row(101),
  ]);
  arrivals.prime("empty");
  assert.deepEqual(arrivals.update([row(0, "empty")]), [row(0, "empty")]);
  arrivals.reset();
  assert.deepEqual(arrivals.update([row(999)]), []);
});

test("tail easing is refresh-rate independent and never overshoots", () => {
  const at60 = tailStep(0, 100, 16);
  const at120 = tailStep(tailStep(0, 100, 8), 100, 8);
  assert.ok(Math.abs(at60 - at120) < 0.00001);
  assert.ok(at60 > 0 && at60 < 100);
  assert.equal(tailStep(99.6, 100, 16), 100);
  assert.ok(tailStep(100, 50, 16) > 50);
});

function surface(t) {
  let now = 0;
  let nextFrame = 0;
  const pending = new Map();
  t.mock.method(globalThis.performance, "now", () => now);
  const originalRequest = globalThis.requestAnimationFrame;
  const originalCancel = globalThis.cancelAnimationFrame;
  globalThis.requestAnimationFrame = (fn) => {
    pending.set(++nextFrame, fn);
    return nextFrame;
  };
  globalThis.cancelAnimationFrame = (id) => pending.delete(id);
  t.after(() => {
    globalThis.requestAnimationFrame = originalRequest;
    globalThis.cancelAnimationFrame = originalCancel;
  });
  const attributes = new Set();
  const element = {
    scrollTop: 0,
    scrollHeight: 1200,
    clientHeight: 400,
    scrollTo({ top }) {
      this.scrollTop = top;
    },
    toggleAttribute: (name, on) => (on ? attributes.add(name) : attributes.delete(name)),
    removeAttribute: (name) => attributes.delete(name),
  };
  let state = { away: false, unseen: 0 };
  let reduced = false;
  const controller = new TailScroll(
    element,
    (next) => {
      state = next;
    },
    () => reduced,
  );
  t.after(() => controller.dispose());
  const frame = (ms = 16) => {
    now += ms;
    const tasks = [...pending.values()];
    pending.clear();
    tasks.forEach((fn) => fn(now));
    controller.onScroll();
  };
  const settle = () => {
    let attempts = 0;
    while (pending.size && attempts++ < 150) frame();
    assert.equal(pending.size, 0, "animation must settle");
  };
  return {
    element,
    controller,
    frame,
    settle,
    pending,
    attributes,
    get state() {
      return state;
    },
    reduce: () => {
      reduced = true;
      controller.resize();
    },
  };
}

test("initial landing is immediate; appends and growing text follow smoothly", (t) => {
  const s = surface(t);
  s.controller.update(true, 0);
  assert.equal(s.element.scrollTop, 800);
  s.element.scrollHeight += 200;
  s.controller.update(true, 1);
  assert.equal(s.element.scrollTop, 800, "no append-time jump");
  s.frame();
  assert.ok(s.element.scrollTop > 800 && s.element.scrollTop < 1000);
  s.element.scrollHeight += 300;
  s.controller.resize();
  assert.equal(s.pending.size, 1, "streaming retargets the same animation");
  s.settle();
  assert.equal(s.element.scrollTop, 1300);
  assert.deepEqual(s.state, { away: false, unseen: 0 });
});

test("opening settles team rows and measured previews before reveal; live arrivals still ease", t => {
  const s = surface(t);
  s.controller.update(true, 0, true);
  s.element.scrollHeight += 300;
  s.controller.update(true, 0, true);
  assert.equal(s.element.scrollTop, 1100);
  s.element.scrollHeight -= 120; // long preview finishes clamping
  s.controller.resize();
  assert.equal(s.element.scrollTop, 980);
  assert.equal(s.pending.size, 0, "opening layout never animates underneath the loader");
  s.controller.update(true, 0, false);
  s.element.scrollHeight += 200;
  s.controller.update(true, 1);
  assert.equal(s.element.scrollTop, 980, "normal live-follow resumes after reveal");
  s.settle();
  assert.equal(s.element.scrollTop, 1180);
});

test("opening never pulls a search landing to the foot", t => {
  const s = surface(t);
  s.controller.pause();
  s.element.scrollTop = 200;
  s.controller.update(true, 0, true);
  s.element.scrollHeight += 300;
  s.controller.resize();
  assert.equal(s.element.scrollTop, 200);
  assert.equal(s.pending.size, 0);
});

test("live-follow settles when the browser rounds scroll positions to device pixels", (t) => {
  const s = surface(t);
  let top = 0;
  Object.defineProperty(s.element, "scrollTop", {
    get: () => top,
    set: value => { top = Math.round(value * 2) / 2; },
  });
  s.controller.update(true, 0);
  s.element.scrollHeight += 203;
  s.controller.update(true, 1);
  for (let i = 0; i < 150 && s.pending.size > 0; i += 1) s.frame(8);
  assert.equal(s.element.scrollTop, 1003);
  assert.equal(s.pending.size, 0, "rounding must not leave an endless near-bottom animation");
});

test("scrolling up shows Latest without new rows, and incoming rows do not move the reader", (t) => {
  const s = surface(t);
  s.controller.update(true, 0);
  s.controller.pause();
  s.element.scrollTop = 500;
  s.controller.onScroll();
  assert.deepEqual(s.state, { away: true, unseen: 0 });
  s.element.scrollHeight += 200;
  s.controller.update(true, 3);
  s.controller.resize();
  assert.equal(s.element.scrollTop, 500);
  assert.deepEqual(s.state, { away: true, unseen: 3 });
  assert.equal(s.pending.size, 0);
  s.controller.latest();
  s.settle();
  assert.equal(s.element.scrollTop, 1000);
  assert.deepEqual(s.state, { away: false, unseen: 0 });
});

test("interrupting a return-to-latest retains unread messages and cancels pending frames", (t) => {
  const s = surface(t);
  s.controller.update(true, 0);
  s.element.scrollTop = 300;
  s.controller.onScroll();
  s.element.scrollHeight += 500;
  s.controller.update(true, 4);
  s.controller.latest();
  s.frame();
  s.controller.pause();
  const stopped = s.element.scrollTop;
  s.frame();
  assert.equal(s.element.scrollTop, stopped);
  assert.deepEqual(s.state, { away: true, unseen: 4 });
  assert.equal(s.pending.size, 0);
});

test("history landing and search anchors preserve position; returning manually resumes follow", (t) => {
  const s = surface(t);
  s.controller.pause();
  s.controller.update(true, 0);
  assert.equal(s.element.scrollTop, 0);
  s.element.scrollTop = 450;
  s.controller.onScroll();
  s.element.scrollHeight += 1000;
  s.element.scrollTop += 1000; // the head-of-document's anchor compensation
  s.controller.update(true, 0);
  s.controller.onScroll();
  assert.equal(s.element.scrollTop, 1450);
  assert.deepEqual(s.state, { away: true, unseen: 0 });
  s.element.scrollTop = 1800;
  s.controller.onScroll();
  assert.equal(s.controller.isFollowing, true);
  assert.equal(s.state.away, false);
});

test("reduced motion lands immediately, also when changed during an animation", (t) => {
  const s = surface(t);
  s.controller.update(true, 0);
  s.element.scrollHeight += 300;
  s.controller.update(true, 1);
  s.frame();
  s.reduce();
  assert.equal(s.element.scrollTop, 1100);
  assert.equal(s.pending.size, 0);
  s.element.scrollTop = 200;
  s.controller.onScroll();
  s.element.scrollHeight += 100;
  s.controller.update(true, 2);
  assert.equal(s.element.scrollTop, 200, "reduced motion still respects the paused reader");
  s.controller.latest();
  assert.equal(s.element.scrollTop, 1200);
});

test("queued native keyboard scroll events cannot cancel an explicit return to latest", (t) => {
  const s = surface(t);
  s.controller.update(true, 0);
  s.controller.pause();
  s.element.scrollTop = 500;
  s.controller.onScroll();
  s.controller.latest();
  s.element.scrollTop = 460; // a queued PageUp position arrives before our first frame
  s.controller.onScroll();
  s.frame();
  s.element.scrollTop = 440; // native keyboard easing can span several more frames
  s.controller.onScroll();
  s.settle();
  assert.equal(s.element.scrollTop, 800);
  assert.equal(s.controller.isFollowing, true);
  assert.equal(s.state.away, false);
});

test("a document anchor can still interrupt live-follow after its animation begins", (t) => {
  const s = surface(t);
  s.controller.update(true, 0);
  s.element.scrollHeight += 500;
  s.controller.update(true, 1);
  s.frame();
  s.element.scrollTop = 300; // a subthread's scrollIntoView landing
  s.controller.onScroll();
  assert.equal(s.controller.isFollowing, false);
  assert.equal(s.pending.size, 0);
  assert.equal(s.element.scrollTop, 300);
});

test("unmount stops work and a reconnect does not treat an empty frame as reaching the bottom", (t) => {
  const s = surface(t);
  s.controller.update(true, 0);
  s.element.scrollTop = 200;
  s.controller.onScroll();
  s.controller.update(false, 0);
  s.element.scrollHeight = 0;
  s.element.scrollTop = 0;
  s.controller.onScroll();
  assert.equal(s.controller.isFollowing, false);
  s.element.scrollHeight = 1400;
  s.controller.update(true, 0);
  assert.equal(s.state.away, true);
  s.controller.latest();
  s.controller.dispose();
  assert.equal(s.pending.size, 0);
  assert.equal(s.attributes.has("data-following"), false);
});
