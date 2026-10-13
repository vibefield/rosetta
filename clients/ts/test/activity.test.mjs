// RosettaClient.activity: the window and its narrowing ride the query string
// exactly as `/api/activity` reads them, absent options stay absent, and the
// bearer rides along — the route spans sessions, so a grant never opens it.
import assert from "node:assert/strict";
import { test } from "node:test";

import { PROTOCOL_VERSION, RosettaClient } from "../dist/client.js";

const BASE = "http://127.0.0.1:4641";

function fakeFetch(body) {
  const calls = [];
  const fetchFn = async (url, init) => {
    calls.push({ url, init });
    return new Response(JSON.stringify(body), {
      status: 200,
      headers: { "x-rosetta-protocol-version": String(PROTOCOL_VERSION) },
    });
  };
  return { calls, fetchFn };
}

const ANSWER = {
  since_ms: 1_000,
  until_ms: 86_401_000,
  gap_ms: 900_000,
  sessions: [],
  truncated: false,
};

test("activity asks for the window, its sessions comma-joined, and nothing it was not given", async () => {
  const { calls, fetchFn } = fakeFetch(ANSWER);
  const client = new RosettaClient({
    baseUrl: BASE,
    fetchFn,
    credential: { kind: "bearer", token: "t".repeat(32) },
  });
  assert.deepEqual(await client.activity({ sinceMs: 1_000 }), ANSWER);
  assert.equal(calls[0].url, `${BASE}/api/activity?since_ms=1000`);
  assert.equal(new Headers(calls[0].init.headers).get("authorization"), `Bearer ${"t".repeat(32)}`);
  await client.activity({
    sinceMs: 1_000,
    untilMs: 86_401_000,
    sessions: ["s1", "s 2"],
    gapMs: 60_000,
    limit: 5,
  });
  const url = new URL(calls[1].url);
  assert.equal(url.pathname, "/api/activity");
  assert.deepEqual(Object.fromEntries(url.searchParams), {
    since_ms: "1000",
    until_ms: "86401000",
    sessions: "s1,s 2",
    gap_ms: "60000",
    limit: "5",
  });
});
