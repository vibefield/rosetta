import assert from "node:assert/strict";
import { test } from "node:test";
import { createSessionLeases } from "../renderer/src/lib/session-leases.ts";

function recordingPort(answer = (id) => ({ id })) {
  const calls = [];
  return {
    calls,
    port: {
      open(id) { calls.push(["open", id]); return answer(id); },
      close(id) { calls.push(["close", id]); },
    },
  };
}

test("one open per session however many hold it, and one close after the last release", () => {
  const { calls, port } = recordingPort();
  const leases = createSessionLeases(port);
  const first = leases.acquire("s1");
  const second = leases.acquire("s1");
  assert.equal(first.client, second.client);
  assert.deepEqual(calls, [["open", "s1"]]);
  first.release();
  first.release();
  assert.deepEqual(calls, [["open", "s1"]], "a repeated release is not a second holder leaving");
  second.release();
  assert.deepEqual(calls, [["open", "s1"], ["close", "s1"]]);
  leases.acquire("s1");
  assert.deepEqual(calls, [["open", "s1"], ["close", "s1"], ["open", "s1"]], "a later reader opens afresh");
});

test("a port that answers later hands the client over when it settles", async () => {
  let resolve;
  const { port } = recordingPort(() => new Promise((yes) => { resolve = yes; }));
  const leases = createSessionLeases(port);
  const lease = leases.acquire("s2");
  assert.equal(lease.client, null);
  const client = { id: "s2" };
  resolve(client);
  assert.equal(await lease.ready, client);
  assert.equal(lease.client, client);
});

test("a refusal reaches the reader, and the session still closes", async () => {
  const { calls, port } = recordingPort(() => Promise.reject(new Error("no ticket")));
  const leases = createSessionLeases(port);
  await assert.rejects(leases.run("s3", async () => "never"), /no ticket/);
  assert.deepEqual(calls, [["open", "s3"], ["close", "s3"]]);
  const { calls: thrown, port: throwing } = recordingPort(() => { throw new Error("refused at once"); });
  await assert.rejects(createSessionLeases(throwing).run("s4", async () => "never"), /refused at once/);
  assert.deepEqual(thrown, [["open", "s4"], ["close", "s4"]]);
});

test("a one-shot read shares an open session and leaves it open for its holder", async () => {
  const { calls, port } = recordingPort();
  const leases = createSessionLeases(port);
  const held = leases.acquire("s5");
  assert.equal(await leases.run("s5", async (client) => client.id), "s5");
  assert.deepEqual(calls, [["open", "s5"]]);
  held.release();
  assert.deepEqual(calls, [["open", "s5"], ["close", "s5"]]);
});
