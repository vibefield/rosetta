import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, readdir, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createArtifactThumbnailService } from "../electron/artifact-thumbnails.mjs";

const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a6foAAAAASUVORK5CYII=", "base64");
test("thumbnail generation is serialized, deduplicated, and reused across app restarts", async () => {
  const cacheDir = await mkdtemp(path.join(os.tmpdir(), "rosetta-thumbnail-cache-"));
  try {
    let calls = 0, active = 0, maximum = 0;
    const service = createArtifactThumbnailService({ cacheDir, render: async () => {
      calls++; maximum = Math.max(maximum, ++active);
      await new Promise((resolve) => setTimeout(resolve, 5));
      active--; return png;
    } });
    const images = await Promise.all([service.get("one"), service.get("one"), service.get("two")]);
    assert.equal(new Set(images).size, 1);
    assert.equal(calls, 2); assert.equal(maximum, 1);
    service.dispose();
    const restarted = createArtifactThumbnailService({ cacheDir, render: () => assert.fail("cached thumbnail was rendered again") });
    assert.equal(await restarted.get("one"), images[0]);
    assert.equal((await readdir(cacheDir)).length, 2);
    assert.equal(await restarted.get({ path: "/etc/passwd" }), null);
    assert.equal(await restarted.get("x".repeat(17 * 1024 * 1024)), null);
  } finally { await rm(cacheDir, { recursive: true, force: true }); }
});

test("the disk cache is bounded and failed rendering is retryable", async () => {
  const cacheDir = await mkdtemp(path.join(os.tmpdir(), "rosetta-thumbnail-cache-"));
  try {
    let calls = 0;
    const service = createArtifactThumbnailService({ cacheDir, maxEntries: 1, render: async () => ++calls === 1 ? null : png });
    assert.equal(await service.get("one"), null);
    assert.ok(await service.get("one"));
    assert.ok(await service.get("two"));
    assert.equal((await readdir(cacheDir)).length, 1);
    assert.equal(calls, 3);
  } finally { await rm(cacheDir, { recursive: true, force: true }); }
});
