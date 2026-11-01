import assert from "node:assert/strict";
import test from "node:test";
import { ApiRequestError } from "../../ts/client.ts";
import { loadAllArtifacts } from "../../room/src/components/artifacts/load-all.ts";
import { ArtifactThumbnailCache } from "../../room/src/components/artifacts/thumbnails.ts";

const entry = (id) => ({ publication: { id, session_id: "session" }, asset: { digest: id } });
test("loads every metadata page without a button or a fixed overall limit", async () => {
  const all = Array.from({ length: 205 }, (_, i) => entry(String(i)));
  const requests = [], updates = [];
  await loadAllArtifacts({ async list(request) {
    requests.push(request);
    const at = Number(request.cursor ?? 0);
    return { entries: all.slice(at, at + 100), next_cursor: at + 100 < all.length ? String(at + 100) : null, canonical_ready: true };
  } }, { q: "design", agent: "claude-code" }, new AbortController().signal, (page) => updates.push(page));
  assert.equal(requests.length, 3);
  assert.ok(requests.every((r) => r.q === "design" && r.agent === "claude-code" && r.limit === 100));
  assert.deepEqual(updates.at(-1).entries, all);
});

test("an expired scan restarts without duplicating or mixing old rows", async () => {
  let calls = 0, latest;
  await loadAllArtifacts({ async list(request) {
    calls++;
    if (calls === 1) return { entries: [entry("old")], next_cursor: "old-page", canonical_ready: true };
    if (calls === 2) throw new ApiRequestError(410, "cursor_expired", "updated");
    assert.equal(request.cursor, null);
    return { entries: [entry("new")], next_cursor: null, canonical_ready: true };
  } }, {}, new AbortController().signal, (page) => { latest = page; });
  assert.deepEqual(latest.entries, [entry("new")]);
});

test("cancellation prevents a stale filter response from updating the grid", async () => {
  const controller = new AbortController();
  await assert.rejects(loadAllArtifacts({ async list() {
    controller.abort();
    return { entries: [entry("stale")], next_cursor: null };
  } }, {}, controller.signal, () => assert.fail("stale update")), { name: "AbortError" });
});

test("a repeatedly invalidated scan fails visibly instead of looping forever", async () => {
  let calls = 0;
  await assert.rejects(loadAllArtifacts({ async list() {
    calls++; throw new ApiRequestError(410, "cursor_expired", "updated");
  } }, {}, new AbortController().signal, () => {}), { code: "cursor_expired" });
  assert.equal(calls, 3);
});

test("static thumbnails serialize work, cache by content, and skip cancelled cards", async () => {
  const cache = new ArtifactThumbnailCache(), controller = new AbortController();
  let reads = 0, active = 0, maximum = 0, renders = 0;
  const port = { async read() { reads++; return new TextEncoder().encode("<h1>Review</h1>"); } };
  const render = async () => {
    renders++; maximum = Math.max(maximum, ++active);
    await new Promise((resolve) => setTimeout(resolve, 5));
    active--; return "data:image/png;base64,cGl4ZWxz";
  };
  const jobs = [cache.read(port, entry("a"), render, controller.signal), cache.read(port, entry("b"), render, controller.signal)];
  const cancelled = new AbortController();
  const skipped = cache.read(port, entry("c"), render, cancelled.signal);
  cancelled.abort();
  await assert.rejects(skipped, { name: "AbortError" });
  await Promise.all(jobs);
  await cache.read(port, entry("a"), render, controller.signal);
  assert.deepEqual({ reads, renders, maximum }, { reads: 2, renders: 2, maximum: 1 });
});
