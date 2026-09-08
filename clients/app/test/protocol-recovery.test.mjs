import assert from "node:assert/strict";
import { test } from "node:test";
import { CursorExpiredError, RosettaClient, PROTOCOL_VERSION } from "../../ts/client.ts";
import { harness, fakeClock, deferred, response, row } from "./support/protocol.mjs";

test("a disconnect before the first snapshot requests a snapshot again", (t) => {
  const clock = fakeClock(t);
  const h = harness(t);
  h.socket.open({ epoch: 1, seq: 100 });
  h.socket.close();
  clock.tick(500);
  assert.equal(new URL(h.sockets[1].url).search, "");
});

test("an interrupted replay resumes from the last applied checkpoint, including its metadata", (t) => {
  const clock = fakeClock(t);
  const h = harness(t);
  h.socket.open();
  h.socket.snapshot([row("initial")]);
  h.socket.close();
  clock.tick(500);
  const replay = h.sockets[1];
  replay.open({ epoch: 1, seq: 20 });
  replay.frame({
    frame: "mutation",
    stamp: null,
    mutation: { type: "message", ...row("only-11", 11) },
  });
  replay.close();
  clock.tick(1000);
  assert.equal(new URL(h.sockets[2].url).searchParams.get("seq"), "10");
  const complete = h.sockets[2];
  complete.open({ epoch: 1, seq: 20 });
  complete.frame({
    frame: "mutation",
    stamp: null,
    mutation: { type: "message", ...row("only-11", 11) },
  });
  complete.frame({ frame: "caught_up", stamp: { epoch: 1, seq: 20 } });
  assert.equal(h.statuses.at(-1), "live");
  assert.deepEqual(
    h.sub.entries().map((r) => r.timeline_id),
    ["initial", "only-11"],
  );
  complete.close();
  clock.tick(500);
  assert.equal(new URL(h.sockets[3].url).searchParams.get("seq"), "20");
});

test("an empty replay explicitly becomes live", (t) => {
  const clock = fakeClock(t);
  const h = harness(t);
  h.socket.open();
  h.socket.snapshot();
  h.socket.close();
  clock.tick(500);
  h.sockets[1].open({ epoch: 1, seq: 10 });
  assert.equal(h.statuses.at(-1), "syncing");
  h.sockets[1].frame({ frame: "caught_up", stamp: { epoch: 1, seq: 10 } });
  assert.equal(h.statuses.at(-1), "live");
});

test("resync invalidates historical pages, anchors, and late HTTP responses together", async (t) => {
  const late = deferred();
  let requests = 0;
  let signal;
  const h = harness(t, {
    fetchFn: async (_url, init) => {
      signal = init.signal;
      if (++requests === 3) return late.promise;
      return response(
        requests === 1
          ? { entries: [row("old", 1)], cursor: null, watermark: "generation-a" }
          : { entry: row("anchor", 2), watermark: "generation-a" },
      );
    },
  });
  h.socket.open();
  h.socket.snapshot([row("tail", 200)]);
  const page = await h.sub.fetchPage("older");
  h.sub.reveal(page.entries);
  await h.sub.loadEntry("anchor");
  assert.equal(h.sub.entries().length, 3);
  const pending = h.sub.fetchPage("older");
  h.socket.frame({ frame: "resync" });
  assert.equal(signal.aborted, true);
  assert.deepEqual(h.sub.entries(), []);
  h.socket.snapshot([], 0, null, "generation-b");
  late.resolve(response({ entries: [row("stale")], cursor: null, watermark: "generation-a" }));
  await assert.rejects(pending, { name: "AbortError" });
  assert.deepEqual(h.sub.entries(), []);
});

test("held and older rows receive tool and usage patches through the same cache", async (t) => {
  const tool = { status: { value: "running" }, action: { type: "shell" } };
  const old = row("old-tool", 1, { type: "tool_use", tool });
  const waiting = deferred();
  const h = harness(t, { fetchFn: () => waiting.promise });
  h.socket.open();
  h.socket.snapshot([row("tail", 200)]);
  const pending = h.sub.fetchPage("older");
  h.socket.frame({
    frame: "mutation",
    stamp: { epoch: 1, seq: 11 },
    mutation: {
      type: "tool_updated",
      session_id: "s1",
      timeline_id: "old-tool",
      tool: { ...tool, status: { value: "succeeded" } },
      at: 20,
    },
  });
  waiting.resolve(response({ entries: [old], cursor: null, watermark: "generation-a" }));
  const page = await pending;
  assert.equal(h.sub.entries().length, 1, "held history remains hidden");
  h.socket.frame({
    frame: "mutation",
    stamp: { epoch: 1, seq: 12 },
    mutation: {
      type: "usage",
      session_id: "s1",
      timeline_id: "old-tool",
      tokens: { output: 7 },
      model: "latest",
      ts: 20,
    },
  });
  h.sub.reveal(page.entries);
  assert.equal(h.sub.entries()[0].body.tool.status.value, "succeeded");
  assert.equal(h.sub.entries()[0].usage.output, 7);
  assert.equal(h.sub.entries()[0].model, "latest");
});

test("the snapshot cursor directly fetches the older page without repeating the tail", async (t) => {
  const urls = [];
  let snapshot;
  const h = harness(
    t,
    {
      fetchFn: async (url) => {
        urls.push(new URL(url));
        return response({ entries: [], cursor: null, watermark: "generation-a" });
      },
    },
    {
      onSnapshot: (page) => {
        snapshot = page;
      },
    },
  );
  h.socket.open();
  h.socket.snapshot([row("tail")], 10, "opaque-cursor");
  await h.sub.fetchPage(snapshot.cursor);
  assert.equal(urls.length, 1);
  assert.equal(urls[0].searchParams.get("cursor"), "opaque-cursor");
});

test("a page from a different generation forces a fresh snapshot", async (t) => {
  const h = harness(t, {
    fetchFn: async () =>
      response({ entries: [row("wrong")], cursor: null, watermark: "rewritten" }),
  });
  h.socket.open();
  h.socket.snapshot([row("tail")]);
  await assert.rejects(h.sub.fetchPage("older"), CursorExpiredError);
  assert.deepEqual(h.sub.entries(), []);
  assert.equal(new URL(h.sockets[1].url).search, "");
});

test("metadata updates batch notifications and retain unaffected published arrays", (t) => {
  const clock = fakeClock(t);
  const h = harness(t, { notificationIntervalMs: 16 });
  const changes = [];
  const meta = h.client.subscribeMeta({ onChange: (state) => changes.push(state) });
  t.after(() => meta.close());
  const socket = h.sockets[1];
  socket.open(null);
  const sessions = Array.from({ length: 10000 }, (_, i) => ({ id: `s${i}`, updated_at: i }));
  socket.frame({
    frame: "meta_snapshot",
    runtimes: [{ id: "runtime" }],
    projects: [{ id: "p", name: "project" }],
    sessions,
  });
  const initial = changes[0];
  for (let i = 0; i < 100; i++)
    socket.frame({
      frame: "mutation",
      stamp: null,
      mutation: { type: "session", id: `s${i}`, updated_at: 20000 + i },
    });
  assert.equal(changes.length, 1);
  clock.tick(16);
  assert.equal(changes.length, 2);
  assert.equal(changes[1].sessions[0].id, "s99");
  assert.equal(initial.sessions[0].id, "s9999", "previously published state was not mutated");
  assert.equal(changes[1].runtimes, initial.runtimes);
  assert.equal(changes[1].projects, initial.projects);
});

test("incompatible WebSocket versions stop with an actionable error", (t) => {
  const clock = fakeClock(t);
  const h = harness(t);
  h.socket.open(null, PROTOCOL_VERSION - 1);
  assert.equal(h.errors[0].code, "incompatible_protocol");
  assert.equal(h.statuses.at(-1), "closed");
  clock.tick(30000);
  assert.equal(h.sockets.length, 1);
});

test("a new daemon origin aborts HTTP and reconnects existing subscriptions", async (t) => {
  let signal;
  const waiting = deferred();
  const h = harness(t, {
    fetchFn: (_url, init) => {
      signal = init.signal;
      return waiting.promise;
    },
  });
  h.socket.open();
  h.socket.snapshot([row("old")]);
  const request = h.client.status();
  h.client.setBaseUrl("http://127.0.0.1:5001");
  assert.equal(signal.aborted, true);
  assert.equal(h.sockets[1].url, "ws://127.0.0.1:5001/ws/sessions/s1");
  assert.deepEqual(h.sub.entries(), []);
  // A late frame from the old socket cannot mutate the replacement cache.
  h.socket.snapshot([row("late")]);
  assert.deepEqual(h.sub.entries(), []);
  waiting.reject(new DOMException("Aborted", "AbortError"));
  await assert.rejects(request, { name: "AbortError" });
});

test("HTTP requests enforce version, cancellation, and a deadline", async () => {
  const mismatch = new RosettaClient({
    WebSocketImpl: class {},
    fetchFn: async () => response({}, PROTOCOL_VERSION - 1),
  });
  await assert.rejects(mismatch.status(), (error) => error.code === "incompatible_protocol");
  const client = new RosettaClient({
    WebSocketImpl: class {},
    requestTimeoutMs: 10,
    fetchFn: (_url, { signal }) =>
      new Promise((_resolve, reject) => {
        signal.addEventListener("abort", () => reject(signal.reason), { once: true });
      }),
  });
  const keepAlive = setTimeout(() => {}, 1000);
  try {
    await assert.rejects(client.status(), { name: "TimeoutError" });
  } finally {
    clearTimeout(keepAlive);
  }
  const abort = new AbortController();
  const pending = client.status(abort.signal);
  abort.abort();
  await assert.rejects(pending, { name: "AbortError" });
});

test("a stalled replacement snapshot times out and requests a new generation", (t) => {
  const clock = fakeClock(t);
  const h = harness(t);
  h.socket.open();
  h.socket.snapshot([row("old")]);
  h.socket.frame({ frame: "resync" });
  clock.tick(15_000);
  assert.equal(h.socket.closed, true);
  clock.tick(500);
  assert.equal(new URL(h.sockets[1].url).search, "");
  assert.deepEqual(h.sub.entries(), []);
});

test("collaboration streams retain only boundary rows and replace observations on resync", (t) => {
  const clock = fakeClock(t);
  const h = harness(t);
  const changes = [];
  const sub = h.client.subscribeCollaboration("worker", {
    onChange: (state) => changes.push(state),
  });
  t.after(() => sub.close());
  const socket = h.sockets[1];
  socket.open(null);
  const boundary = row("boundary", 1, { type: "collaboration", event: { kind: "message" } });
  socket.frame({ frame: "collaboration_snapshot", entries: [boundary], response_runtimes: [] });
  socket.frame({
    frame: "mutation",
    stamp: null,
    mutation: { type: "message", ...row("private") },
  });
  assert.equal(changes.length, 1);
  socket.frame({
    frame: "response_runtime",
    observation: { response_id: "r1", model: "model", effort: null },
  });
  assert.equal(changes.at(-1).entries[0], changes[0].entries[0]);
  assert.equal(changes.at(-1).response_runtimes.length, 1);
  socket.frame({ frame: "resync" });
  socket.frame({ frame: "collaboration_snapshot", entries: [], response_runtimes: [] });
  assert.deepEqual(changes.at(-1), { entries: [], response_runtimes: [] });
  socket.close();
  clock.tick(500);
  assert.equal(h.sockets.at(-1).url, "ws://127.0.0.1:4640/ws/collaboration/worker");
});
