import assert from "node:assert/strict";
import { test } from "node:test";
import { isAwaitingConsent, isCensusRunning, isIngestActive } from "../../room/src/lib/ingest.ts";

const progress = (stamp, phase = "backfilling") => ({
  phase,
  metadata_ready_at: stamp,
  // The final runtime can still be queued between metadata and streaming.
  runtimes: [{ state: "streaming" }, { state: "queued" }],
  projects: [],
});

test("a pause before consent keeps the import controls visible until resume", () => {
  const waiting = progress(null, "awaiting_consent");
  assert.equal(isAwaitingConsent(waiting), true);
  assert.equal(isAwaitingConsent({ ...waiting, run: { state: "running" } }), true);
  for (const state of ["pausing", "paused", "failed"]) {
    const stopped = { ...waiting, run: { state } };
    assert.equal(isAwaitingConsent(stopped), false);
    assert.equal(isIngestActive(stopped), true);
  }
});

test("the first-pass stamp releases lists while transcript ingest remains active", () => {
  const before = progress(null);
  assert.equal(isCensusRunning(before), true);
  for (const stamp of [0, 2_600]) {
    for (const phase of ["backfilling", "restoring", "finalizing", "publishing"]) {
      const after = progress(stamp, phase);
      assert.equal(isCensusRunning(after), false);
      assert.equal(isIngestActive(after), true);
    }
  }
});

test("an explicit pending stamp is authoritative even if runtime meters look finished", () => {
  const pending = progress(null);
  pending.runtimes = [{ state: "streaming", files_skimmed: 10, files_total: 10 }];
  assert.equal(isCensusRunning(pending), true);
  assert.equal(isCensusRunning({ ...pending, runtimes: [] }), true);
});

test("older daemons keep phase-based list readiness and a warm live catalog stays usable", () => {
  const legacy = progress(undefined);
  delete legacy.metadata_ready_at;
  assert.equal(isCensusRunning(legacy), true);
  assert.equal(isCensusRunning({ ...legacy, phase: "metadata_ready" }), false);
  assert.equal(isCensusRunning({ ...legacy, runtimes: [{ state: "streaming" }] }), false);
  assert.equal(isCensusRunning(progress(null, "live")), false);
  assert.equal(isCensusRunning(null), false);
});
