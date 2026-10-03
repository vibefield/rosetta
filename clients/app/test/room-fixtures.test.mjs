import assert from "node:assert/strict";
import { test } from "node:test";
import { createDemoPorts, createRoomDemo, demoActivityOrder, demoProjectViewModels } from "../../room/src/fixtures/index.ts";
import { isOptimizedImageAsset } from "../../room/src/lib/image-media.ts";

const NOW = Date.UTC(2026, 9, 3, 12);

test("the demo's rows hold together: every session in a project, every row in a session", () => {
  const demo = createRoomDemo(NOW);
  const projects = new Set(demo.meta.projects.map((project) => project.id));
  const runtimes = new Set(demo.meta.runtimes.map((runtime) => runtime.id));
  for (const session of demo.meta.sessions) {
    assert.equal(projects.has(session.project_id), true, session.id);
    assert.equal(runtimes.has(session.runtime_id), true, session.id);
    assert.equal(session.activity.last_at <= NOW, true, "nothing in the future");
  }
  const sessions = new Set(demo.meta.sessions.map((session) => session.id));
  for (const [sessionId, rows] of demo.timelines) {
    assert.equal(sessions.has(sessionId), true);
    for (const row of rows) assert.equal(row.session_id, sessionId);
  }
  assert.equal(demo.meta.projects.length, 6);
});

test("its images are the optimized members the room will draw, each distinct", () => {
  const demo = createRoomDemo(NOW);
  const attachments = demo.timelines.get("s0")[0].body.attachments;
  assert.equal(attachments.length, 2);
  for (const attachment of attachments) assert.equal(isOptimizedImageAsset(attachment.source.asset), true);
  assert.notEqual(attachments[0].source.asset.digest, attachments[1].source.asset.digest);
});

test("its times are relative to now, so it never goes stale", () => {
  const later = createRoomDemo(NOW + 30 * 86_400_000);
  const now = createRoomDemo(NOW);
  assert.equal(later.meta.sessions[0].updated_at - now.meta.sessions[0].updated_at, 30 * 86_400_000);
  assert.equal(now.stats.get("p0").heat.at(-1).tokens >= 0, true);
});

test("its view models and order are what the columns draw", () => {
  const demo = createRoomDemo(NOW);
  const models = demoProjectViewModels(demo);
  assert.deepEqual(models.map((model) => model.row.name), ["vibe-field", "rosetta", "puppeteer", "infinite-canvas-engine", "ghosttea", "truffle"]);
  assert.equal(models[0].rootSessionCount, 8);
  const order = demoActivityOrder(demo);
  assert.equal(order.sessions.size, demo.meta.sessions.length);
});

test("its ports answer the room: rows, figures, a session's transcript, a search, an image", async () => {
  const demo = createRoomDemo(NOW);
  const ports = createDemoPorts(demo);
  assert.equal(ports.host, undefined, "no desktop: a host adds its own");
  const meta = await new Promise((resolve) => ports.catalog.subscribeMeta({ onChange: resolve }));
  assert.equal(meta.sessions.length, 11);
  assert.equal((await ports.catalog.status()).canonicalReady, true);
  assert.equal((await ports.catalog.projectStats("p0")).contribution.length, 8);
  const rows = await new Promise((resolve) => ports.sessions.open("s0").subscribeSession("s0", { onChange: resolve }));
  assert.equal(rows.length, 11);
  const found = await ports.catalog.search({ q: "frame", limit: 200, project: null, kind: null, agent: null });
  assert.deepEqual(found.hits.map((hit) => hit.timeline_id), ["s0-t1", "s0-t8"]);
  assert.match(found.hits[0].snippet, /\[frame\]/);
  assert.match(ports.assets.imageAssetUrl(demo.timelines.get("s0")[0].body.attachments[0].source.asset, "tile"), /^data:image\/svg\+xml,/);
});
