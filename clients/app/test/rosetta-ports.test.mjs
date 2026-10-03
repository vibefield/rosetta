import assert from "node:assert/strict";
import { test } from "node:test";
import { RosettaClient } from "../../ts/client.ts";
import { catalogStatusOf } from "../renderer/src/lib/catalog-status.ts";
import { assetsOver, catalogOver, sameOriginAssetUrl, sessionsOver } from "../renderer/src/host/rosetta-ports.ts";

test("a daemon status reads as the room's catalog status, missing blocks as absent", () => {
  const ingest = { phase: "live", runtimes: [], projects: [] };
  assert.deepEqual(catalogStatusOf({
    canonical_ready: true,
    search: { readiness: "ready", progress: { stage: "documents", documents_done: 1, documents_total: 2 } },
    daemon: { started_at: 5, uptime_ms: 9, ingest },
  }), {
    canonicalReady: true, searchReadiness: "ready",
    searchProgress: { stage: "documents", documents_done: 1, documents_total: 2 },
    ingest, startedAt: 5, uptimeMs: 9,
  });
  assert.deepEqual(catalogStatusOf({ canonical_ready: false, search: { readiness: "building" }, daemon: null }), {
    canonicalReady: false, searchReadiness: "building", searchProgress: null, ingest: null, startedAt: null, uptimeMs: 0,
  });
});

test("the catalog folds a project's heat and contribution into one read over the window", async () => {
  const calls = [];
  const signal = new AbortController().signal;
  const client = {
    heatmap: async (...args) => { calls.push(["heatmap", ...args]); return { days: ["day"] }; },
    contribution: async (...args) => { calls.push(["contribution", ...args]); return { rows: ["row"] }; },
  };
  const window = { sinceMs: 10, offsetMinutes: -60 };
  assert.deepEqual(await catalogOver(client).projectStats("p1", window, signal), { heat: ["day"], contribution: ["row"] });
  assert.deepEqual(calls, [["heatmap", "p1", window, signal], ["contribution", "p1", signal]]);
});

test("the catalog's search asks the daemon for the sheet's facets and nothing else", async () => {
  let asked;
  const client = { search: async (request, signal) => { asked = { request, signal }; return { hits: [] }; } };
  const signal = new AbortController().signal;
  await catalogOver(client).search({ q: "ports", limit: 200, project: "p1", kind: null, agent: "codex" }, signal);
  assert.deepEqual(asked.request, {
    q: "ports", limit: 200, session: null, sessions: null, prefix: null, runtime: null,
    agent: "codex", project: "p1", kind: null, is_error: null, after_ts: null, before_ts: null,
  });
  assert.equal(asked.signal, signal);
});

test("the host's words and its route changes reach the room beside the client's own", () => {
  const listeners = { client: new Set(), host: new Set() };
  const client = { onConnectionChange: (l) => { listeners.client.add(l); return () => listeners.client.delete(l); } };
  let words = null;
  const catalog = catalogOver(client, {
    unavailable: () => words,
    onChange: (l) => { listeners.host.add(l); return () => listeners.host.delete(l); },
  });
  assert.equal(catalog.unavailable(), null);
  words = "The daemon stopped.";
  assert.equal(catalog.unavailable(), "The daemon stopped.");
  let heard = 0;
  const stop = catalog.onConnectionChange(() => { heard += 1; });
  for (const l of [...listeners.client, ...listeners.host]) l();
  assert.equal(heard, 2);
  stop();
  assert.equal(listeners.client.size + listeners.host.size, 0);
});

test("every session opens on the one client, and closing it is nothing", () => {
  const client = {};
  const sessions = sessionsOver(client);
  assert.equal(sessions.open("s1"), client);
  assert.equal(sessions.open("s2"), client);
  assert.equal(sessions.close("s1"), undefined);
});

test("assets keep the daemon's own addresses, and an attachment names only that daemon", () => {
  const client = new RosettaClient({ baseUrl: "http://127.0.0.1:4640" });
  const assets = assetsOver(client);
  const asset = { url: "/api/assets/abc.webp", digest: "abc", media_type: "image/webp", byte_len: 1 };
  assert.equal(assets.imageAssetUrl(asset, "full"), client.imageAssetUrl(asset, "full"));
  assert.equal(assets.attachmentAssetUrl(asset), "http://127.0.0.1:4640/api/assets/abc.webp");
  assert.equal(assets.attachmentAssetUrl({ ...asset, url: "https://example.com/abc.webp" }), null);
  assert.equal(sameOriginAssetUrl({ baseUrl: "not a url" }, asset), null);
});
