// The order project figures hydrate in after publication, and how a failed
// project is asked again (ux:X6).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  FIGURE_RETRY_CAP_MS,
  failedFiguresLine,
  figureOrder,
  figureRetryDelay,
} from "../../room/src/lib/figure-order.ts";
import { dataHooks, microtasks } from "./support/hooks.mjs";
import { fakeClock } from "./support/protocol.mjs";

describe("figureOrder", () => {
  const drawn = [
    { id: "d", onScreen: false },
    { id: "b", onScreen: true },
    { id: "a", onScreen: true },
    { id: "c", onScreen: false },
  ];

  it("asks the selected project first, then the on-screen cards, then the rest as drawn, then undrawn by id", () => {
    assert.deepEqual(figureOrder(["a", "b", "c", "d", "z", "y"], "c", drawn), ["c", "b", "a", "d", "y", "z"]);
  });

  it("names every id once, and only ids it was given", () => {
    assert.deepEqual(figureOrder(["a", "c"], "b", drawn), ["a", "c"]);
    assert.deepEqual(figureOrder(["a", "b"], null, []), ["a", "b"]);
    assert.deepEqual(figureOrder([], "a", drawn), []);
  });
});

describe("figureRetryDelay", () => {
  it("starts at 8 s, doubles, and stops at two minutes", () => {
    assert.deepEqual([1, 2, 3, 4, 5].map(figureRetryDelay), [8_000, 16_000, 32_000, 64_000, 120_000]);
    assert.equal(figureRetryDelay(40), FIGURE_RETRY_CAP_MS);
    assert.equal(figureRetryDelay(0), 8_000);
  });
});

describe("failedFiguresLine", () => {
  it("counts projects", () => {
    assert.equal(failedFiguresLine(1), "figures for 1 project failed · retrying");
    assert.equal(failedFiguresLine(1200), "figures for 1,200 projects failed · retrying");
  });
});

describe("useProjectStats", () => {
  // The hook's timers are window's, as in the renderer (this file runs in
  // its own process, so the global stays).
  globalThis.window ??= globalThis;

  it("asks the priority's first, then its order, four at a time", async (t) => {
    const clock = fakeClock(t);
    const asked = [];
    const client = { projectStats: (id) => { asked.push(id); return new Promise(() => {}); } };
    const hook = dataHooks(client)("useProjectStats", ["a", "b", "c", "d", "e", "f"], true,
      { first: "e", order: () => ["c", "a"] });
    t.after(() => hook.close());
    hook.render();
    await microtasks();
    clock.tick(0);
    assert.deepEqual(asked, ["e", "c", "a", "b"]);
  });

  it("asks a failed project again on its own, with backoff, and reports it until it lands", async (t) => {
    const clock = fakeClock(t);
    const asked = [];
    let failing = true;
    const client = {
      projectStats: (id) => {
        asked.push(id);
        return id === "b" && failing ? Promise.reject(new Error("boom")) : Promise.resolve({ heat: [], contribution: [] });
      },
    };
    const hook = dataHooks(client)("useProjectStats", ["a", "b", "c"], true);
    t.after(() => hook.close());
    hook.render();
    await microtasks();
    let figures = hook.render();
    assert.deepEqual([...figures.stats.keys()].sort(), ["a", "c"]);
    assert.deepEqual([...figures.failed], ["b"]);
    asked.length = 0;
    clock.tick(7_999);
    assert.deepEqual(asked, []);
    clock.tick(1);
    await microtasks();
    assert.deepEqual(asked, ["b"]);
    clock.tick(15_999);
    assert.deepEqual(asked, ["b"]);
    failing = false;
    clock.tick(1);
    await microtasks();
    figures = hook.render();
    assert.deepEqual(asked, ["b", "b"]);
    assert.deepEqual([...figures.stats.keys()].sort(), ["a", "b", "c"]);
    assert.equal(figures.failed.size, 0);
  });
});
