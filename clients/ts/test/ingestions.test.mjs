import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiRequestError, PROTOCOL_VERSION, RosettaClient } from "../dist/client.js";

test("cold controls preserve pinned intent and credentials across a lost reply", async () => {
  const calls = [];
  const client = new RosettaClient({ baseUrl: "http://127.0.0.1:4641",
    credential: { kind: "bearer", token: "first-token" },
    fetchFn: async (url, init) => {
      calls.push({ url, init });
      if (calls.length === 1) throw new TypeError("lost response");
      return new Response(JSON.stringify({ id: "ing_test", state: "paused" }), {
        headers: { "x-rosetta-protocol-version": String(PROTOCOL_VERSION) },
      });
    },
  });
  const command = { expected_revision: 7, indexId: "idx_test", idempotencyKey: "one-user-intent" };
  await assert.rejects(client.ingestions.pause("ing_test", command));
  client.setCredential({ kind: "bearer", token: "replacement-token" });
  await client.ingestions.pause("ing_test", command);
  for (const { url, init } of calls) {
    assert.equal(url, "http://127.0.0.1:4641/api/ingestions/ing_test/pause");
    assert.equal(init.method, "POST");
    assert.equal(new Headers(init.headers).get("x-rosetta-index-id"), "idx_test");
    assert.equal(new Headers(init.headers).get("idempotency-key"), "one-user-intent");
    assert.deepEqual(JSON.parse(init.body), { expected_revision: 7 });
  }
  assert.equal(new Headers(calls[1].init.headers).get("authorization"), "Bearer replacement-token");
});

test("revision conflicts retain typed details for intentional reconciliation", async () => {
  const client = new RosettaClient({ fetchFn: async () => new Response(JSON.stringify({ error: {
    code: "revision_conflict", message: "run changed", details: { ingestion_id: "ing_test", current_revision: 9 },
  } }), { status: 409, headers: { "x-rosetta-protocol-version": String(PROTOCOL_VERSION) } }) });
  await assert.rejects(client.ingestions.resume("ing_test", {
    expected_revision: 7, indexId: "idx_test", idempotencyKey: "key",
  }), (error) => error instanceof ApiRequestError && error.code === "revision_conflict" &&
    error.details.current_revision === 9 && error.details.ingestion_id === "ing_test");
});
