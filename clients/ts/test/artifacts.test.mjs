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
