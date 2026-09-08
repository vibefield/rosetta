import assert from "node:assert/strict";
import { test } from "node:test";
import { activityPhase, progressBytes, progressDuration, sampleActivity } from "../renderer/src/lib/ingest-activity.ts";
import { dataHooks, microtasks } from "./support/hooks.mjs";
import { deferred, fakeClock } from "./support/protocol.mjs";

const runtime = (overrides = {}) => ({ kind: "codex", state: "streaming", files_total: 100,
  files_skimmed: 100, sessions_total: 80, bytes_total: 1_000_000, bytes_streamed: 400_000,
  records: 300, ...overrides });
const feed = (overrides = {}) => ({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime()] },
  canonicalReady: false, searchReadiness: "building", searchProgress: null, connectionError: null,
  startedAt: 1, uptimeMs: 5_000, receivedAt: 1_000, ...overrides });

test("the reading percentage belongs to reading; organizing and saving never claim overall completion", () => {
  const reading = activityPhase(feed());
  assert.equal(reading.fraction, .4);
  assert.equal(reading.step, 2);
  for (const phase of ["finalizing", "publishing"]) {
    const view = activityPhase(feed({ ingest: { phase, runtimes: [runtime({ state: "done" })], projects: [] } }));
    assert.equal(view.active, true);
    assert.equal(view.fraction, null);
    assert.equal(view.step, 3);
  }
  const organizing = activityPhase(feed({ ingest: { phase: "backfilling", runtimes: [runtime({ state: "applying" })], projects: [] } }));
  assert.equal(organizing.key, "organizing");
  assert.equal(organizing.fraction, null);
});

test("metadata uses live discovery counters even before project rows arrive", () => {
  const view = activityPhase(feed({ ingest: { phase: "backfilling", projects: [],
    runtimes: [runtime({ state: "skimming", files_skimmed: 25, sessions_total: 19 })] } }));
  assert.equal(view.fraction, .25);
  assert.match(view.detail, /19 sessions found/);
  assert.equal(view.current, 25);
});

test("unknown snapshot sizes and empty archives are indeterminate, and growing sources clamp", () => {
  for (const runtimes of [[runtime({ bytes_total: 0 })], [runtime(), runtime({ bytes_total: 0 })]]) {
    assert.equal(activityPhase(feed({ ingest: { phase: "backfilling", projects: [], runtimes } })).fraction, null);
  }
  assert.equal(activityPhase(feed({ ingest: { phase: "backfilling", projects: [], runtimes: [runtime({ bytes_streamed: 2_000_000 })] } })).fraction, 1);
  assert.equal(progressBytes(0), "0 B");
  assert.equal(progressDuration(-100), "0s");
  assert.equal(progressDuration(125_000), "2m 5s");
});

test("search remains visible after live ingest and validation has its own non-percentage stage", () => {
  const live = feed({ canonicalReady: true, ingest: { phase: "live", projects: [], runtimes: [runtime({ state: "done" })] },
    searchProgress: { stage: "indexing", documents_done: 250, documents_total: 1_000 } });
  assert.equal(activityPhase(live).fraction, .25);
  assert.equal(activityPhase(live).step, 4);
  assert.equal(activityPhase({ ...live, searchProgress: { ...live.searchProgress, stage: "validating" } }).fraction, null);
  assert.equal(activityPhase({ ...live, searchReadiness: "ready" }).key, "ready");
  assert.equal(activityPhase({ ...live, searchReadiness: "failed" }).key, "search_failed");
});

test("throughput smooths observed work, expires when counts pause, and resets across restarts and retries", () => {
  let previous = null;
  const sample = (at, value, startedAt = 1) => {
    const next = feed({ receivedAt: at, startedAt,
      ingest: { phase: "backfilling", projects: [], runtimes: [runtime({ bytes_streamed: value })] } });
    previous = sampleActivity(previous, next, activityPhase(next));
    return previous;
  };
  assert.equal(sample(0, 0).rate, null);
  assert.equal(sample(1_000, 100_000).rate, 100_000);
  assert.equal(sample(2_000, 400_000).rate, 200_000);
  assert.equal(sample(6_500, 400_000).rate, null);
  assert.equal(sample(7_000, 10).rate, null);
  assert.equal(sample(8_000, 100_000, 2).rate, null);
  for (let at = 9_000; at < 60_000; at += 500) sample(at, at * 100, 2);
  assert.ok(previous.points.length <= 17);
});

test("recovery counts restored work without inventing a total, and ordinary live repair stays quiet", () => {
  const restoring = feed({ ingest: { phase: "restoring", projects: [], runtimes: [runtime({ records: 800 })] } });
  assert.equal(activityPhase(restoring).key, "restoring");
  assert.equal(activityPhase(restoring).fraction, null);
  assert.equal(activityPhase(restoring).amount, "800 items restored");
  const live = feed({ canonicalReady: true, ingest: { phase: "live", projects: [], runtimes: [] }, searchReadiness: "catching-up" });
  assert.equal(activityPhase(live), null);
});

test("status polls twice per second through search, preserves progress on failure, and idles only when both are ready", async (t) => {
  const clock = fakeClock(t);
  const oldWindow = globalThis.window;
  globalThis.window = globalThis;
  const requests = [];
  const client = { onConnectionChange: () => () => {}, status(signal) {
    const request = { signal, ...deferred() }; requests.push(request); return request.promise;
  } };
  const hook = dataHooks(client)("useDaemonStatus");
  t.after(async () => {
    hook.close(); await microtasks();
    if (oldWindow === undefined) delete globalThis.window; else globalThis.window = oldWindow;
  });
  const status = { canonical_ready: false, search: { readiness: "building", progress: null },
    daemon: { started_at: 1, uptime_ms: 1_000, ingest: feed().ingest } };
  hook.render();
  requests[0].resolve(status); await microtasks(); hook.render();
  clock.tick(499); assert.equal(requests.length, 1);
  clock.tick(1); assert.equal(requests.length, 2);
  requests[1].resolve({ ...status, canonical_ready: true, daemon: { ...status.daemon, ingest: { ...status.daemon.ingest, phase: "live" } } });
  await microtasks(); hook.render(); clock.tick(500); assert.equal(requests.length, 3);
  requests[2].reject(new Error("connection lost")); await microtasks();
  assert.equal(hook.render().ingest.phase, "live");
  assert.equal(hook.render().connectionError, "connection lost");
  clock.tick(1_000); assert.equal(requests.length, 4);
  requests[3].resolve({ ...status, canonical_ready: true, search: { readiness: "ready" } });
  await microtasks(); assert.equal(hook.render().connectionError, null);
  clock.tick(29_999); assert.equal(requests.length, 4);
  clock.tick(1); assert.equal(requests.length, 5);
  hook.close(); assert.equal(requests[4].signal.aborted, true);
  requests[4].resolve(status); await microtasks(); clock.tick(60_000);
  assert.equal(requests.length, 5);
});

test("a stalled status request is aborted and retries instead of freezing indefinitely", async (t) => {
  const clock = fakeClock(t);
  const oldWindow = globalThis.window;
  globalThis.window = globalThis;
  const signals = [];
  const hook = dataHooks({ onConnectionChange: () => () => {}, status(signal) {
    signals.push(signal);
    return new Promise((_, reject) => signal.addEventListener("abort", () => reject(new Error("aborted"))));
  } })("useDaemonStatus");
  t.after(async () => {
    hook.close(); await microtasks();
    if (oldWindow === undefined) delete globalThis.window; else globalThis.window = oldWindow;
  });
  hook.render(); clock.tick(5_000); await microtasks();
  assert.equal(signals[0].aborted, true);
  assert.match(hook.render().connectionError, /timed out/);
  clock.tick(1_000); assert.equal(signals.length, 2);
});
