// The cold-import simulator keeps the daemon's rules (room/src/fixtures/
// cold-import.ts): its archive has the MacBook trace's shape, its schedule
// walks newest trees first with the flip cadence and caps, its mirror
// withholds Pending activity until publication, and it does nothing unless
// stepped. cold-import.electron.mjs drives the room with it; this pins the
// simulator itself, so a room assertion never rests on a drifted fixture.
import assert from "node:assert/strict";
import { test } from "node:test";
import { RosettaClient } from "../../ts/client.ts";
import { createColdImport } from "../../room/src/fixtures/index.ts";
import { createColdCorpus } from "../../room/src/fixtures/cold-import-corpus.ts";

const NOW = Date.UTC(2026, 9, 2, 12);
const sum = (rows, key) => rows.reduce((total, row) => total + row[key], 0);

test("the archive has the MacBook trace's shape, and every kind of row at any scale", () => {
  const corpus = createColdCorpus({ now: NOW });
  const kinds = {};
  for (const kind of corpus.kind.values()) kinds[kind] = (kinds[kind] ?? 0) + 1;
  assert.equal(corpus.sessions.length, 8_612);
  assert.equal(corpus.trees.length, 1_139);
  assert.equal(corpus.trees.reduce((n, tree) => n + tree.members.length, 0), 8_062);
  assert.equal(corpus.projects.length, 119);
  assert.equal(corpus.trees.filter((tree) => tree.big).length, 40);
  assert.equal(corpus.trees[0].bytes, 668_350_304, "the newest tree is the trace's giant");
  assert.equal(corpus.trees[0].big, true);
  for (const kind of ["root", "child", "placeholder", "never-rostered-root", "never-rostered-child"]) assert.ok(kinds[kind] > 0, kind);
  assert.ok(corpus.sessions.some((row) => row.project_id === null && row.parent_session_id === null), "an unassigned bucket");
  assert.ok(corpus.trees.some((tree) => tree.runtimeId === corpus.runtimes[1].id), "some codex trees");
  assert.equal(corpus.trees.filter((tree) => tree.dirty).length, 5, "dirty trees, whose root flips Streaming");
  // Rows are what the metadata pass publishes: Pending, no activity.
  assert.ok(corpus.sessions.every((row) => row.stream_state === "pending" && row.activity === null));
  for (const row of corpus.sessions.filter((s) => corpus.kind.get(s.id) === "placeholder")) {
    assert.deepEqual([row.project_id, row.cwd, row.title, row.summary, row.delegation], [null, null, null, null, null]);
    assert.ok((corpus.children.get(row.id) ?? []).length > 0, "a placeholder has children");
  }
  for (const scale of [0.05, 0.1, 0.25]) {
    const small = createColdCorpus({ now: NOW, scale });
    assert.ok(new Set(small.kind.values()).size === 5 && small.trees.some((tree) => tree.bound) && small.trees.some((tree) => tree.dirty), `scale ${scale}`);
  }
});

test("the archive is seeded: same inputs, same archive; ids never depend on now", () => {
  const shape = (corpus) => JSON.stringify(corpus.sessions.map((row) => [row.id, row.parent_session_id, row.title, row.updated_at - corpus.now]));
  assert.equal(shape(createColdCorpus({ now: NOW, scale: 0.2 })), shape(createColdCorpus({ now: NOW + 86_400_000, scale: 0.2 })));
  assert.notEqual(shape(createColdCorpus({ now: NOW, scale: 0.2, seed: 2 })), shape(createColdCorpus({ now: NOW, scale: 0.2 })));
});

test("trees flip newest first in each lane, at most once per 100 ms, within the caps", () => {
  const { controller: c } = createColdImport({ now: NOW, consent: "granted" });
  const batches = c.batches().filter((batch) => batch.kind === "tree");
  for (const runtime of c.corpus.runtimes) {
    const own = batches.filter((batch) => batch.runtimeId === runtime.id);
    own.forEach((batch, i) => {
      if (i > 0) assert.ok(batch.at - own[i - 1].at >= 100, "D9 cadence");
      assert.ok(batch.trees.length <= 64);
      assert.ok(batch.trees.length === 1 || batch.sessions.length <= 512);
      // Buffered tails, modelled at 2.5% of a tree's bytes, capped at 16 MiB.
      assert.ok(batch.trees.length === 1 || batch.trees.reduce((n, tree) => n + Math.round(tree.bytes * 0.025), 0) <= 16 * 1024 * 1024);
    });
    for (const big of [false, true]) {
      const lane = c.corpus.trees.filter((tree) => tree.runtimeId === runtime.id && tree.big === big && !tree.bound);
      for (let i = 1; i < lane.length; i++) assert.ok(c.flipAt(lane[i - 1].id) <= c.flipAt(lane[i].id), "newest first within a lane");
    }
  }
  // The giant newest tree waits in the big lane behind older small ones.
  assert.ok(c.phaseAt("newest-giant") > c.phaseAt("first-flip"));
  // Bound trees flip only at their producer's close.
  for (const tree of c.corpus.trees.filter((t) => t.bound)) assert.equal(c.flipAt(tree.id), c.phaseAt("runtime-close"));
  const phases = ["metadata", "first-flip", "reading-done", "runtime-close", "finalizing", "publishing", "canonical", "reveal", "live",
    "search-indexing", "search-optimizing", "search-validating", "search-publishing", "search-ready"];
  for (let i = 1; i < phases.length; i++) assert.ok(c.phaseAt(phases[i - 1]) < c.phaseAt(phases[i]), `${phases[i - 1]} before ${phases[i]}`);
});

test("nothing happens unless stepped, and the clock only moves forward", async () => {
  const sim = createColdImport({ now: NOW, scale: 0.05, consent: "granted" });
  let deliveries = 0;
  sim.ports.catalog.subscribeMeta({ onChange: () => { deliveries += 1; } });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(deliveries, 1, "only the subscription's own snapshot");
  assert.equal(sim.controller.clock, 0);
  sim.controller.advanceTo(1_000);
  assert.throws(() => sim.controller.advanceTo(999), /only moves forward/);
});

test("metadata, flips and the end reach subscribers as the hub sends them", async () => {
  const sim = createColdImport({ now: NOW, consent: "granted" });
  const c = sim.controller;
  const seen = [];
  sim.ports.catalog.subscribeMeta({ onChange: (state, source) => seen.push({ source, sessions: state.sessions.length }), onStatus: () => {} });
  await Promise.resolve();
  c.advanceToPhase("metadata");
  // Over the hub's 1,024 queue the burst is a resync and a snapshot.
  assert.deepEqual(c.log.meta.filter((m) => m.why === "metadata").map((m) => m.source), ["snapshot", "mutation"]);
  assert.equal(seen.at(-1).sessions, 8_612);
  // The SDK's order: updated_at descending, then id.
  const order = c.meta().sessions;
  for (let i = 1; i < order.length; i++) assert.ok(order[i - 1].updated_at > order[i].updated_at || (order[i - 1].updated_at === order[i].updated_at && order[i - 1].id < order[i].id));
  c.advanceToPhase("newest-giant");
  const readable = c.meta().sessions.filter((row) => row.stream_state !== "pending");
  // A dirty tree's root flips Streaming, the rest of the tree Indexed.
  for (const tree of c.corpus.trees.filter((t) => c.flipAt(t.id) <= c.clock)) {
    for (const id of tree.members) assert.equal(c.streamState(id), tree.dirty && id === tree.id ? "streaming" : "indexed");
  }
  assert.ok(readable.some((row) => row.stream_state === "streaming"), "a Streaming root by the giant's flip");
  const status = c.status();
  // The board counts every planned session done once its tree flipped.
  assert.equal(sum(status.ingest.projects, "sessions_done"), readable.length);
  assert.equal(sum(status.ingest.projects, "sessions_total"), 8_062);
  c.advanceToPhase("search-ready");
  assert.equal(sum(c.status().ingest.projects, "sessions_done"), 8_062);
  assert.equal(c.meta().sessions.filter((row) => row.stream_state === "pending").length, 550, "the never-rostered and the placeholders");
});

test("the mirror withholds Pending activity and stops rollups at Pending members until publication", () => {
  const { controller: c } = createColdImport({ now: NOW, scale: 0.3, consent: "granted" });
  c.advanceToPhase("runtime-close");
  const rows = new Map(c.meta().sessions.map((row) => [row.id, row]));
  for (const row of rows.values()) if (row.stream_state === "pending") assert.equal(row.activity, null, "a Pending row carries no activity");
  const placeholders = [...rows.values()].filter((row) => c.kindOf(row.id) === "placeholder");
  assert.ok(placeholders.length > 0 && placeholders.every((row) => row.activity === null));
  c.advanceToPhase("reveal");
  const revealed = new Map(c.meta().sessions.map((row) => [row.id, row]));
  // reveal_pending: the placeholders now roll up their readable children.
  assert.ok(placeholders.every((row) => revealed.get(row.id).activity !== null));
  assert.equal(c.log.meta.filter((m) => m.why === "reveal").reduce((n, m) => n + m.rows, 0), placeholders.length);
});

test("status reports each phase as the daemon does, canonical readiness while still publishing", () => {
  const before = createColdImport({ now: NOW, scale: 0.1 }).controller;
  assert.equal(before.status().ingest.phase, "discovering");
  before.advanceTo(400);
  assert.equal(before.status().ingest.phase, "awaiting_consent");
  assert.equal(before.phaseAt("metadata"), null, "nothing is scheduled before consent");
  const { controller: c } = createColdImport({ now: NOW, scale: 0.1, consent: "granted" });
  const at = (phase) => {
    c.advanceToPhase(phase);
    const status = c.status();
    return [status.ingest.phase, status.canonicalReady, status.ingest.runtimes.map((row) => row.state).join(",")];
  };
  assert.deepEqual(at("metadata"), ["backfilling", false, "streaming,streaming"]);
  assert.deepEqual(at("reading-done"), ["backfilling", false, "preparing,done"]);
  assert.deepEqual(at("runtime-close"), ["backfilling", false, "done,done"]);
  assert.deepEqual(at("finalizing"), ["finalizing", false, "done,done"]);
  assert.deepEqual(at("publishing"), ["publishing", false, "done,done"]);
  assert.deepEqual(at("canonical"), ["publishing", true, "done,done"]);
  assert.deepEqual(at("live"), ["live", true, "done,done"]);
  assert.ok(c.status().ingest.metadata_ready_at === NOW + c.phaseAt("metadata"));
  // Live before the index reports progress (a simplification: the daemon's
  // worker wakes first and races), then every stage of the build.
  assert.equal(c.status().searchProgress, null);
  for (const stage of ["indexing", "optimizing", "validating", "publishing"]) {
    c.advanceToPhase(`search-${stage}`);
    assert.deepEqual([c.status().searchReadiness, c.status().searchProgress.stage], ["building", stage]);
  }
  c.advanceToPhase("search-ready");
  assert.deepEqual([c.status().searchReadiness, c.status().searchProgress], ["ready", null]);
});

test("consent places the schedule; aggregates are not ready and search has no hits before publication", async () => {
  const sim = createColdImport({ now: NOW, scale: 0.1 });
  const c = sim.controller;
  c.advanceTo(400);
  assert.equal(sim.ports.catalog.status instanceof Function, true);
  assert.deepEqual(await sim.consent.enableRuntimes(["claude-code", "codex"]), { enabled: ["claude-code", "codex"] });
  await assert.rejects(sim.consent.enableRuntimes(["codex"]), /already granted/);
  assert.equal((await sim.ports.catalog.status()).ingest.phase, "awaiting_consent", "the grant registers; the skim moves the phase");
  c.advanceToPhase("metadata");
  assert.equal((await sim.ports.catalog.status()).ingest.phase, "backfilling");
  await assert.rejects(sim.ports.catalog.projectStats("any", { sinceMs: 0, offsetMinutes: 0 }), (error) => error.code === "not_ready");
  const query = c.meta().sessions.find((row) => row.title !== null).title.split(":")[0];
  assert.deepEqual((await sim.ports.catalog.search({ q: query, limit: 10, project: null, kind: null, agent: null })).hits, []);
  c.advanceToPhase("canonical");
  const published = await sim.ports.catalog.status();
  assert.deepEqual([published.canonicalReady, published.ingest.phase, published.searchReadiness], [true, "publishing", "building"]);
  assert.ok(Array.isArray((await sim.ports.catalog.projectStats("__rosetta_unassigned__", { sinceMs: 0, offsetMinutes: 0 })).contribution));
  assert.deepEqual((await sim.ports.catalog.search({ q: query, limit: 10, project: null, kind: null, agent: null })).hits, [], "no hits before the index's first generation");
  c.advanceToPhase("search-ready");
  const ready = await sim.ports.catalog.status();
  assert.deepEqual([ready.ingest.phase, ready.searchReadiness, ready.ingest.run.state], ["live", "ready", "succeeded"]);
  assert.ok((await sim.ports.catalog.search({ q: query, limit: 10, project: null, kind: null, agent: null })).hits.length > 0);
  assert.deepEqual(await sim.ports.catalog.boost("any"), { queued: false });
});

test("a held status ask is answered at release, with the state then", async () => {
  const sim = createColdImport({ now: NOW, scale: 0.05, consent: "granted" });
  const c = sim.controller;
  c.holdStatus();
  const asked = sim.ports.catalog.status();
  assert.equal(c.heldStatus(), 1);
  c.advanceToPhase("metadata");
  assert.equal(c.releaseStatus(), 1);
  assert.equal((await asked).ingest.metadata_ready_at, NOW + c.phaseAt("metadata"));
  const controller = new AbortController();
  c.holdStatus();
  const abandoned = sim.ports.catalog.status(controller.signal);
  controller.abort();
  await assert.rejects(abandoned, (error) => error.name === "AbortError");
  assert.equal(c.releaseStatus(), 0);
});

test("an open session resyncs at its flip, or waits, or never hears, as asked", async () => {
  const sim = createColdImport({ now: NOW, scale: 0.1, consent: "granted" });
  const c = sim.controller;
  c.advanceToPhase("metadata");
  const roots = c.meta().sessions.filter((row) => row.parent_session_id === null && c.kindOf(row.id) === "root" && !c.treeOf(row.id).bound)
    .sort((a, b) => c.flipAt(a.id) - c.flipAt(b.id));
  const open = (id) => {
    const events = [];
    const client = sim.ports.sessions.open(id);
    client.subscribeSession(id, {
      onReset: () => events.push("reset"), onSnapshot: (page) => events.push(`snapshot:${page.entries.length}`),
      onStatus: (status) => events.push(status), onChange: () => {}, onTimelineMetaChange: () => {},
    });
    return events;
  };
  const [first, second, third] = roots.filter((row, i, all) => i === 0 || c.flipAt(row.id) !== c.flipAt(all[i - 1].id));
  const a = open(first.id);
  await Promise.resolve();
  assert.deepEqual(a, ["snapshot:0", "live"], "a Pending session's snapshot is authoritative and empty");
  c.advanceTo(c.flipAt(first.id));
  assert.equal(a[2], "reset");
  assert.equal(a[3], "resyncing");
  assert.match(a[4], /^snapshot:[1-9]/);
  c.resyncMode = "manual";
  const b = open(second.id);
  await Promise.resolve();
  c.advanceTo(c.flipAt(second.id));
  assert.equal(c.streamState(second.id), "indexed");
  assert.deepEqual(b, ["snapshot:0", "live"], "held: the catalog flipped first");
  assert.equal(c.deliverResyncs(), 1);
  assert.match(b.at(-2), /^snapshot:[1-9]/);
  c.resyncMode = "drop";
  const d = open(third.id);
  await Promise.resolve();
  c.advanceTo(c.flipAt(third.id));
  assert.deepEqual(d, ["snapshot:0", "live"], "dropped: nothing arrives");
});

test("a resumed build takes each runtime in turn, then opens everything at publication, metadata before resyncs", async () => {
  const sim = createColdImport({ now: NOW, scale: 0.1, course: "resumed" });
  const c = sim.controller;
  const [claude, codex] = c.corpus.runtimes.map((runtime) => c.runtimeAt(runtime.id));
  // Per runtime, in registry order: replay, skim, catch-up, done.
  assert.ok(claude.replay.start < claude.replay.end && claude.replay.end === claude.skimStart && claude.skimStart < claude.metaPublic);
  assert.ok(claude.metaPublic < claude.readEnd && claude.readEnd < claude.close && claude.close <= codex.replay.start);
  assert.ok(codex.replay.end < codex.metaPublic && codex.close < c.phaseAt("finalizing"));
  const look = (t) => {
    c.advanceTo(t);
    const status = c.status();
    return {
      phase: status.ingest.phase, states: status.ingest.runtimes.map((row) => row.state).join(","),
      bytesTotal: status.ingest.runtimes.map((row) => row.bytes_total), runtimes: c.meta().runtimes.length,
      readable: c.meta().sessions.filter((row) => row.stream_state !== "pending").length,
      done: sum(status.ingest.projects, "sessions_done"), metadataReady: status.ingest.metadata_ready_at !== null,
      canonical: status.canonicalReady, run: status.ingest.run.state,
    };
  };
  const census = (runtime) => c.corpus.trees.filter((tree) => tree.runtimeId === runtime.runtime.id).reduce((n, tree) => n + tree.bytes, 0);
  const quiet = { readable: 0, done: 0, canonical: false, run: "running" };
  assert.deepEqual(look(claude.replay.start), { phase: "restoring", states: "streaming,queued", bytesTotal: [census(claude), census(codex)], runtimes: 0, metadataReady: false, ...quiet });
  assert.deepEqual(look(claude.skimStart), { phase: "backfilling", states: "skimming,queued", bytesTotal: [census(claude), census(codex)], runtimes: 0, metadataReady: false, ...quiet });
  // Public at its skim, still Pending; the board and the meter know no byte totals.
  assert.deepEqual(look(claude.metaPublic), { phase: "backfilling", states: "streaming,queued", bytesTotal: [0, census(codex)], runtimes: 1, metadataReady: false, ...quiet });
  assert.ok(c.status().ingest.projects.length > 0 && c.status().ingest.projects.every((row) => row.bytes_total === 0));
  const root = c.meta().sessions.find((row) => row.parent_session_id === null && c.kindOf(row.id) === "root");
  const events = [];
  sim.ports.sessions.open(root.id).subscribeSession(root.id, {
    onReset: () => events.push("reset"), onSnapshot: (page) => events.push(`snapshot:${page.entries.length}`),
    onStatus: () => {}, onChange: () => {}, onTimelineMetaChange: () => {},
  });
  await Promise.resolve();
  // Its runtime's end resyncs it, and it still reads empty; the next
  // runtime's replay begins at once.
  assert.equal(codex.replay.start, claude.close);
  assert.deepEqual(look(claude.close), { phase: "restoring", states: "done,streaming", bytesTotal: [0, census(codex)], runtimes: 1, metadataReady: false, ...quiet });
  assert.deepEqual(events, ["snapshot:0", "reset", "snapshot:0"]);
  assert.deepEqual(look(codex.metaPublic), { phase: "backfilling", states: "done,streaming", bytesTotal: [0, 0], runtimes: 2, metadataReady: true, ...quiet });
  assert.equal(c.status().ingest.metadata_ready_at, NOW + codex.metaPublic, "the last runtime's skim stamps metadata-ready");
  // The withheld commit reaches the store, never the hub; publication's
  // read switch still precedes every flip.
  for (const phase of ["finalizing", "publishing"]) assert.deepEqual(look(c.phaseAt(phase)), { phase, states: "done,done", bytesTotal: [0, 0], runtimes: 2, metadataReady: true, ...quiet });
  assert.deepEqual(look(c.phaseAt("canonical")), { phase: "publishing", states: "done,done", bytesTotal: [0, 0], runtimes: 2, metadataReady: true, ...quiet, canonical: true });
  const before = c.log.trail.length;
  const total = c.corpus.trees.reduce((n, t) => n + t.members.length, 0);
  assert.deepEqual(look(c.phaseAt("reveal")), { phase: "publishing", states: "done,done", bytesTotal: [0, 0], runtimes: 2, metadataReady: true, readable: total, done: total, canonical: true, run: "succeeded" });
  // One whole-surface delivery, then the open session's resync.
  const after = c.log.trail.slice(before).map((entry) => entry.what);
  assert.deepEqual(after, ["meta", "resync"]);
  assert.deepEqual(c.log.meta.filter((m) => m.why === "publication").map((m) => m.source), ["mutation"]);
  assert.deepEqual(events.slice(3), ["reset", `snapshot:${events.at(-1).split(":")[1]}`]);
  assert.match(events.at(-1), /^snapshot:[1-9]/);
  c.setRunState("paused");
  assert.deepEqual([c.status().ingest.run.state, c.status().ingest.run.allowed_actions], ["paused", ["resume"]]);
  // Over the hub's queue the publication arrives as a resync and a snapshot.
  const larger = createColdImport({ now: NOW, scale: 0.25, course: "resumed" }).controller;
  larger.advanceToPhase("reveal");
  assert.deepEqual(larger.log.meta.filter((m) => m.why === "publication").map((m) => m.source), ["snapshot"]);
});

test("the daemon's REST doors answer a RosettaClient as rosettad would", async () => {
  const sim = createColdImport({ now: NOW, scale: 0.05 });
  const c = sim.controller;
  const client = new RosettaClient({ baseUrl: "http://127.0.0.1:4640", fetchFn: sim.daemon.fetch });
  c.advanceTo(400);
  const waiting = await client.status();
  assert.deepEqual([waiting.canonical_ready, waiting.daemon.ingest.phase, waiting.search.readiness], [false, "awaiting_consent", "building"]);
  assert.deepEqual((await client.discoveries()).discoveries.filter((row) => row.installed).map((row) => row.kind), ["claude-code", "codex"]);
  assert.deepEqual(await client.enableRuntimes(["claude-code", "codex"]), { enabled: ["claude-code", "codex"] });
  assert.equal(c.log.trail.at(-1).what, "grant");
  await assert.rejects(client.enableRuntimes(["codex"]), (error) => error.status === 409 && error.code === "invalid_transition");
  c.advanceToPhase("metadata");
  const project = c.meta().projects[0].id;
  await assert.rejects(client.heatmap(project), (error) => error.status === 503 && error.code === "not_ready");
  await assert.rejects(client.usage(project), (error) => error.code === "not_ready");
  const query = c.meta().sessions.find((row) => row.title !== null).title.split(":")[0];
  const request = { q: query, limit: 10, session: null, sessions: null, prefix: null, runtime: null, agent: null, project: null, kind: null, is_error: null, after_ts: null, before_ts: null };
  assert.deepEqual((await client.search(request)).hits, []);
  assert.deepEqual(await client.boost("any"), { queued: false });
  // A held ask that the client abandons rejects as an abort and leaves the hold.
  c.holdStatus();
  const controller = new AbortController();
  const held = client.status(controller.signal);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(c.heldStatus(), 1);
  controller.abort();
  await assert.rejects(held, (error) => error.name === "AbortError");
  assert.equal(c.heldStatus(), 0);
  c.releaseStatus();
  c.advanceToPhase("search-ready");
  const ready = await client.status();
  assert.deepEqual([ready.canonical_ready, ready.daemon.ingest.phase, ready.search.readiness], [true, "live", "ready"]);
  assert.ok(Array.isArray((await client.contribution(project)).rows));
  assert.ok((await client.search(request)).hits.length > 0);
  await assert.rejects(client.runtimes(), (error) => error.status === 404 && error.code === "not_found");
});

test("a host's connection change reaches the room's listeners, on the trail", () => {
  const sim = createColdImport({ now: NOW, scale: 0.05, consent: "granted" });
  let heard = 0;
  const stop = sim.ports.catalog.onConnectionChange(() => { heard += 1; });
  assert.equal(sim.controller.connectionChange(), 1);
  assert.equal(heard, 1);
  assert.equal(sim.controller.log.trail.at(-1).what, "connection-change");
  stop();
  assert.equal(sim.controller.connectionChange(), 0);
});
