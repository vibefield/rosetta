// RosettaClient's credential: `Authorization` on REST, the `rosetta`
// subprotocol pair on WebSocket upgrades, a typed 401 — and, with no
// credential, requests and sockets exactly as before.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import {
  AUTH_REFUSALS,
  PROTOCOL_VERSION,
  RosettaClient,
  UnauthorizedError,
  authorizationHeader,
  webSocketProtocols,
} from "../dist/client.js";

const BASE = "http://127.0.0.1:4641";

function fakeFetch(status = 200, headers = {}) {
  const calls = [];
  const fetchFn = async (url, init) => {
    calls.push({ url, init });
    return new Response(status === 200 ? "{}" : "rosetta: unauthorized\n", {
      status,
      headers: { "x-rosetta-protocol-version": String(PROTOCOL_VERSION), ...headers },
    });
  };
  return { calls, fetchFn };
}

function fakeSockets() {
  const opened = [];
  class FakeSocket {
    constructor(...args) {
      this.args = args;
      opened.push(this);
    }
    close() {}
  }
  return { opened, FakeSocket };
}

test("with no credential, requests and sockets are exactly as before", async () => {
  const { calls, fetchFn } = fakeFetch();
  const { opened, FakeSocket } = fakeSockets();
  const client = new RosettaClient({ baseUrl: BASE, fetchFn, WebSocketImpl: FakeSocket });
  await client.sessions();
  assert.equal("headers" in calls[0].init, false);
  await client.boost("s1");
  assert.deepEqual(calls[1].init.headers, { "content-type": "application/json" });
  const meta = client.subscribeMeta({});
  assert.deepEqual(opened.at(-1).args, [`ws://127.0.0.1:4641/ws/meta`]);
  meta.close();
});

test("a bearer rides Authorization on REST and the subprotocol pair on upgrades", async () => {
  const token = "tok/en=+with-symbols";
  const credential = { kind: "bearer", token };
  const { calls, fetchFn } = fakeFetch();
  const { opened, FakeSocket } = fakeSockets();
  const client = new RosettaClient({ baseUrl: BASE, fetchFn, WebSocketImpl: FakeSocket, credential });
  await client.boost("s1");
  const headers = new Headers(calls[0].init.headers);
  assert.equal(headers.get("authorization"), `Bearer ${token}`);
  assert.equal(headers.get("content-type"), "application/json");
  const sub = client.subscribeSession("s1", {});
  assert.deepEqual(opened.at(-1).args, [
    "ws://127.0.0.1:4641/ws/sessions/s1",
    ["rosetta", `rosetta.bearer.${Buffer.from(token).toString("base64url")}`],
  ]);
  sub.close();
  assert.equal(authorizationHeader(credential), `Bearer ${token}`);
});

test("a grant rides the same carriers, and setCredential swaps or drops it", async () => {
  const grant = "eyJ2IjoxfQ.bWFj";
  const { calls, fetchFn } = fakeFetch();
  const { opened, FakeSocket } = fakeSockets();
  const client = new RosettaClient({
    baseUrl: BASE,
    fetchFn,
    WebSocketImpl: FakeSocket,
    credential: { kind: "grant", grant },
  });
  await client.timeline("s1");
  assert.equal(new Headers(calls[0].init.headers).get("authorization"), `Rosetta-Grant ${grant}`);
  const collaboration = client.subscribeCollaboration("s1", {});
  assert.deepEqual(opened.at(-1).args[1], ["rosetta", `rosetta.grant.${grant}`]);
  collaboration.close();
  assert.deepEqual(webSocketProtocols({ kind: "grant", grant }), ["rosetta", `rosetta.grant.${grant}`]);

  client.setCredential({ kind: "grant", grant: "fresh.grant" });
  await client.timeline("s1");
  assert.equal(new Headers(calls[1].init.headers).get("authorization"), "Rosetta-Grant fresh.grant");
  client.setCredential(null);
  await client.timeline("s1");
  assert.equal("headers" in calls[2].init, false);
  const meta = client.subscribeMeta({});
  assert.equal(opened.at(-1).args.length, 1);
  meta.close();
});

test("the refusal vocabulary is the shared fixture's", () => {
  const fixture = JSON.parse(
    readFileSync(new URL("../../../crates/server/tests/fixtures/grants.json", import.meta.url), "utf8"),
  );
  assert.deepEqual([...AUTH_REFUSALS], fixture.refusals);
});

test("a 401 is an UnauthorizedError naming the daemon's reason", async () => {
  for (const reason of AUTH_REFUSALS) {
    const { fetchFn } = fakeFetch(401, { "x-rosetta-auth": reason });
    const client = new RosettaClient({ baseUrl: BASE, fetchFn });
    await assert.rejects(client.session("s1"), (error) => {
      assert.ok(error instanceof UnauthorizedError);
      assert.equal(error.status, 401);
      assert.equal(error.reason, reason);
      assert.equal(error.code, null);
      assert.match(error.message, new RegExp(reason));
      return true;
    });
  }
  const { fetchFn } = fakeFetch(401, { "x-rosetta-auth": "something-new" });
  const client = new RosettaClient({ baseUrl: BASE, fetchFn });
  await assert.rejects(client.session("s1"), (error) => error.reason === null);
});
