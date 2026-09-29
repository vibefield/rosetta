import assert from "node:assert/strict";
import { test } from "node:test";
import { dataHooks, microtasks } from "./support/hooks.mjs";
import { deferred, fakeClock, harness, response, row } from "./support/protocol.mjs";

test("search aborts superseded requests and ignores their late answers and failures", async (t) => {
  const clock = fakeClock(t);
  const requests = [];
  const client = {
    search: (query, signal) => {
      const request = { query, signal, ...deferred() };
      requests.push(request);
      return request.promise;
    },
  };
  const hook = dataHooks(client)("useSearch", "old");
  t.after(() => hook.close());
  hook.render();
  clock.tick(220);
  hook.render("new");
  assert.equal(requests[0].signal.aborted, true);
  clock.tick(220);
  requests[1].resolve({ hits: ["new"] });
  await microtasks();
  assert.deepEqual(hook.render().response.hits, ["new"]);
  requests[0].resolve({ hits: ["stale"] });
  await microtasks();
  assert.deepEqual(hook.render().response.hits, ["new"]);
  hook.render("third");
  clock.tick(220);
  hook.render("");
  requests[2].reject(new Error("late failure"));
  await microtasks();
  assert.equal(hook.render().error, null);
  assert.equal(hook.render().searching, false);
  assert.equal(requests[2].signal.aborted, true);
});

test("the transcript uses one older-page request and clears history plus anchors on resync", async (t) => {
  const urls = [];
  const h = harness(t, {
    fetchFn: async (url) => {
      urls.push(new URL(url));
      return response(
        url.includes("/anchor")
          ? { entry: row("anchor", 0), watermark: "generation-a" }
          : { entries: [row("older", 1)], cursor: null, watermark: "generation-a" },
      );
    },
  });
  h.sub.close();
  const hook = dataHooks(h.client)("useTimeline", "s1");
  t.after(() => hook.close());
  hook.render();
  const socket = h.sockets[1];
  socket.open();
  socket.snapshot([row("tail", 200)]);
  hook.render().loadEarlier();
  await microtasks();
  assert.equal(urls.length, 1);
  assert.equal(urls[0].searchParams.get("cursor"), "older");
  assert.deepEqual(
    hook.render().rows.map((entry) => entry.timeline_id),
    ["older", "tail"],
  );
  await hook.render().loadAnchor("anchor");
  assert.equal(hook.render().rows.length, 3);
  socket.frame({ frame: "resync" });
  assert.deepEqual(hook.render().rows, []);
  assert.equal(hook.render().snapshotReady, false);
  socket.snapshot([], 0, null, "generation-b");
  assert.deepEqual(hook.render().rows, []);
  assert.equal(hook.render().snapshotReady, true);
  assert.equal(hook.render().earlier.kind, "none");
});

test("a cursor expiry restarts the subscription and preserves the rewrite notice", async (t) => {
  const h = harness(t, {
    fetchFn: async () => response({ entries: [], cursor: null, watermark: "rewritten" }),
  });
  h.sub.close();
  const hook = dataHooks(h.client)("useTimeline", "s1");
  t.after(() => hook.close());
  hook.render();
  h.sockets[1].open();
  h.sockets[1].snapshot([row("tail", 200)]);
  hook.render().loadEarlier();
  await microtasks();
  assert.equal(hook.render().earlierNotice?.kind, "rewritten");
  h.sockets[2].open();
  h.sockets[2].snapshot([], 0, null, "rewritten");
  assert.equal(hook.render().earlierNotice?.kind, "rewritten");
});

test("team roster reconciliation retains existing streams and ignores removed members", async (t) => {
  const subscriptions = [];
  const hook = dataHooks({
    subscribeCollaboration(id, handlers) {
      const sub = {
        id,
        handlers,
        closed: false,
        restarts: 0,
        restart() { this.restarts += 1; handlers.onReset(); },
        close() {
          this.closed = true;
        },
      };
      subscriptions.push(sub);
      return sub;
    },
  })("useTeamTimelines", ["a", "b"]);
  t.after(() => hook.close());
  hook.render();
  const history = (id) => ({ entries: [row(id)], response_runtimes: [] });
  subscriptions.forEach((sub) => sub.handlers.onChange(history(sub.id)));
  assert.equal(hook.render().historyReady, true);
  assert.equal(hook.render(["b", "a", "c"]).historyReady, false);
  assert.equal(subscriptions.length, 3);
  assert.equal(subscriptions[0].closed, false);
  subscriptions[2].handlers.onChange(history("c"));
  assert.equal(hook.render().historyReady, true);
  hook.render(["b", "c"]);
  assert.equal(subscriptions[0].closed, true);
  subscriptions[0].handlers.onChange(history("late"));
  assert.equal(hook.render().rowsBySession.has("a"), false);
  assert.equal(subscriptions.length, 3);
  subscriptions[2].handlers.onReset();
  subscriptions[2].handlers.onError({ message: "Offline" });
  assert.equal(hook.render().historyError, "Offline");
  assert.equal(hook.render().historyReady, false);
  hook.render().retry();
  assert.equal(subscriptions[1].restarts, 0, "healthy members are not restarted");
  assert.equal(subscriptions[2].restarts, 1);
  subscriptions[2].handlers.onChange(history("c"));
  assert.equal(hook.render().historyError, null);
  assert.equal(hook.render().historyReady, true);
  hook.close();
  assert.equal(
    subscriptions.every((sub) => sub.closed),
    true,
  );
});

test('chat boundaries retain cached prefixes for later reveal and exact anchors bypass the visible start', async t => {
  const prefix = row('prefix', 99, { type: 'thinking', text: { type: 'inline', text: 'Before chat' } });
  const h = harness(t, { fetchFn: async url => response(url.includes('/timeline/prefix')
    ? { entry: prefix, watermark: 'generation-a' }
    : { entries: [row('earlier-chat', 98)], cursor: null, watermark: 'generation-a' }) });
  h.sub.close();
  const hook = dataHooks(h.client)('useTimeline', 's1');
  t.after(() => hook.close()); hook.render();
  const socket = h.sockets[1]; socket.open();
  socket.snapshot([prefix, row('chat', 100)]);
  assert.deepEqual(hook.render().rows.map(row => row.timeline_id), ['chat']);
  await hook.render().loadAnchor('prefix');
  assert.deepEqual(hook.render().rows.map(row => row.timeline_id), ['prefix', 'chat']);
  hook.render().loadEarlier(); await microtasks();
  assert.deepEqual(hook.render().rows.map(row => row.timeline_id), ['earlier-chat', 'prefix', 'chat']);
});

test('a live primary reply cannot hide an already visible activity-only snapshot', t => {
  const h = harness(t); h.sub.close();
  const hook = dataHooks(h.client)('useTimeline', 's1');
  t.after(() => hook.close()); hook.render();
  const socket = h.sockets[1]; socket.open();
  socket.snapshot([row('activity', 200, { type: 'thinking', text: { type: 'inline', text: 'Working' } })]);
  socket.frame({ frame: 'mutation', stamp: { epoch: 1, seq: 11 }, mutation: { type: 'message', ...row('reply', 201) } });
  assert.deepEqual(hook.render().rows.map(row => row.timeline_id), ['activity', 'reply']);
});
