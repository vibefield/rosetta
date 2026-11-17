import assert from "node:assert/strict";
import test from "node:test";
import { RosettaClient, PROTOCOL_VERSION } from "../dist/index.js";

test("artifact pagination and captured content preserve filters, credentials, and exact session identity", async () => {
  const calls = [];
  const html = "<button>Review π</button>";
  const client = new RosettaClient({ baseUrl: "http://127.0.0.1:4640", credential: { kind: "bearer", token: "test-token" }, fetchFn: async (url, options) => {
    calls.push({ url: String(url), options });
    const content = String(url).endsWith("/content");
    return new Response(content ? html : JSON.stringify({ entries: [], next_cursor: "opaque cursor", canonical_ready: true }), {
      headers: { "content-type": content ? "application/octet-stream" : "application/json", "x-rosetta-protocol-version": String(PROTOCOL_VERSION) },
    });
  } });
  const page = await client.artifacts({ q: "review & design", agent: "codex", project: "project/a", artifact_id: null, cursor: null, limit: 25 });
  assert.equal(page.next_cursor, "opaque cursor");
  const query = new URL(calls[0].url).searchParams;
  assert.equal(query.get("q"), "review & design");
  assert.equal(query.get("project"), "project/a");
  assert.equal(query.get("agent"), "codex");
  const bytes = await client.readArtifact("child/session", "publication/a");
  assert.equal(new TextDecoder().decode(bytes), html);
  assert.ok(calls[1].url.endsWith("/api/sessions/child%2Fsession/artifacts/publication%2Fa/content"));
  assert.equal(new Headers(calls[1].options.headers).get("authorization"), "Bearer test-token");
});

test("an artifact export posts its request with the bearer and returns the by-digest answer", async () => {
  const calls = [];
  const environment = {
    hosts: { "script-src": ["cdn.jsdelivr.net"], "style-src": [], "font-src": [] },
    variables: { "--color-text-primary": "#252521" },
  };
  const answers = [
    { page: { path: "index.html", blake3: "c".repeat(64), bytes: 15 }, members: [], captured_at: 50, method: "link", environment },
    {
      page: { path: "index.html", blake3: "d".repeat(64), bytes: 1234 },
      members: [],
      captured_at: null,
      method: "copy",
      environment,
      shell: { version: "1", source_blake3: "c".repeat(64), hosts: ["cdn.jsdelivr.net"] },
    },
  ];
  const client = new RosettaClient({ baseUrl: "http://127.0.0.1:4640", credential: { kind: "bearer", token: "test-token" }, fetchFn: async (url, options) => {
    calls.push({ url: String(url), options });
    return new Response(JSON.stringify(answers[calls.length - 1]), {
      headers: { "content-type": "application/json", "x-rosetta-protocol-version": String(PROTOCOL_VERSION) },
    });
  } });

  // The page as itself: the request is the body, exactly.
  const raw = await client.exportArtifact("child/session", "publication/a", { into: "/exports/s1/review" });
  assert.deepEqual(raw, answers[0]);
  assert.ok(calls[0].url.endsWith("/api/sessions/child%2Fsession/artifacts/publication%2Fa/export"));
  assert.equal(calls[0].options.method, "POST");
  assert.deepEqual(JSON.parse(calls[0].options.body), { into: "/exports/s1/review" });
  const headers = new Headers(calls[0].options.headers);
  assert.equal(headers.get("authorization"), "Bearer test-token");
  assert.equal(headers.get("content-type"), "application/json");

  // The room's shell, asked for.
  const shelled = await client.exportArtifact("s1", "p1", { into: "/exports/s1/room", shell: "room" });
  assert.deepEqual(shelled, answers[1]);
  assert.deepEqual(JSON.parse(calls[1].options.body), { into: "/exports/s1/room", shell: "room" });
});
