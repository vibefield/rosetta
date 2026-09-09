import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { test } from "node:test";
import { createDaemonService } from "../electron/daemon-service.mjs";
import { APP_PRIVILEGES, rendererFileUrl } from "../electron/app-protocol.mjs";
import { PROTOCOL_VERSION } from "../../ts/protocol-version.mjs";
import { fakeClock } from "./support/protocol.mjs";

function harness(t) {
  const clock = fakeClock(t);
  const children = [];
  const states = [];
  const service = createDaemonService({
    bin: "/unused/rosettad",
    dbPath: "/private/tmp/rosetta-service-test.sqlite",
    onState: (state) => states.push(state),
    stdout: () => {},
    stderr: () => {},
    spawnProcess: (_bin, args) => {
      const child = new EventEmitter();
      Object.assign(child, {
        pid: 100 + children.length,
        stdout: new EventEmitter(),
        stderr: new EventEmitter(),
        signals: [],
        args,
      });
      child.kill = (signal) => {
        child.signals.push(signal);
        return true;
      };
      child.ready = (port = 5000, protocol_version = PROTOCOL_VERSION) =>
        child.stdout.emit(
          "data",
          JSON.stringify({ event: "ready", origin: `http://127.0.0.1:${port}`, protocol_version }) +
            "\n",
        );
      children.push(child);
      return child;
    },
  });
  t.after(async () => {
    const stopped = service.stop();
    for (const child of children) child.emit("exit", 0, null);
    await stopped;
  });
  return { clock, children, states, service };
}

test("readiness requires the structured versioned event and accepts fragmented stdout", async (t) => {
  const h = harness(t);
  const ready = h.service.start();
  const child = h.children[0];
  assert.ok(child.args.includes("--json"));
  assert.ok(child.args.includes("--announce"));
  child.stdout.emit("data", "serving http://127.0.0.1:9999\n");
  assert.equal(h.service.state().origin, null);
  const greeting = JSON.stringify({
    event: "ready",
    origin: "http://127.0.0.1:5000",
    protocol_version: PROTOCOL_VERSION,
  });
  child.stdout.emit("data", greeting.slice(0, 20));
  child.stdout.emit("data", greeting.slice(20) + "\n");
  assert.equal(await ready, "http://127.0.0.1:5000");
  assert.equal(h.service.state().phase, "ready");
});

test("startup timeout kills and reaps the old child before retrying", async (t) => {
  const h = harness(t);
  const ready = h.service.start();
  const first = h.children[0];
  h.clock.tick(8000);
  assert.equal(await ready, null);
  assert.deepEqual(first.signals, ["SIGINT"]);
  h.clock.tick(10_000);
  assert.deepEqual(first.signals, ["SIGINT", "SIGKILL"]);
  h.clock.tick(60_000);
  assert.equal(h.children.length, 1, "no replacement while the previous child still exists");
  first.ready();
  assert.equal(h.service.state().origin, null, "late readiness cannot revive a timed-out child");
  first.emit("exit", null, "SIGKILL");
  h.clock.tick(500);
  assert.equal(h.children.length, 2);
  h.children[1].ready(5001);
  assert.equal(h.service.state().origin, "http://127.0.0.1:5001");
});

test("a protocol mismatch is terminal and is surfaced to the renderer", async (t) => {
  const h = harness(t);
  const ready = h.service.start();
  h.children[0].ready(5000, PROTOCOL_VERSION - 1);
  assert.equal(await ready, null);
  assert.equal(h.service.state().phase, "failed");
  assert.match(h.service.state().error, /does not match/);
  h.children[0].emit("exit", 0, null);
  h.clock.tick(60_000);
  assert.equal(h.children.length, 1);
});

test("a crash changes origin and graceful shutdown waits for the actual exit", async (t) => {
  const h = harness(t);
  const ready = h.service.start();
  h.children[0].ready();
  await ready;
  h.children[0].emit("exit", 1, null);
  assert.equal(h.service.state().origin, null);
  h.clock.tick(500);
  h.children[1].ready(5002);
  assert.equal(h.service.state().origin, "http://127.0.0.1:5002");
  let finished = false;
  const stopping = h.service.stop().then(() => {
    finished = true;
  });
  await Promise.resolve();
  assert.equal(finished, false);
  assert.deepEqual(h.children[1].signals, ["SIGINT"]);
  h.children[1].emit("exit", 0, null);
  await stopping;
  assert.equal(h.service.state().phase, "stopped");
  h.clock.tick(60_000);
  assert.equal(h.children.length, 2);
});

test("spawn failures finish without an exit event", async (t) => {
  const h = harness(t);
  const ready = h.service.start();
  h.children[0].pid = undefined;
  h.children[0].emit("error", Object.assign(new Error("binary missing"), { code: "ENOENT" }));
  assert.equal(await ready, null);
  assert.equal(h.service.state().phase, "failed");
  h.clock.tick(60_000);
  assert.equal(h.children.length, 1);
});

test("failure to signal a running child cannot spawn an overlapping daemon", async (t) => {
  const h = harness(t);
  const ready = h.service.start();
  h.children[0].ready();
  await ready;
  h.children[0].emit("error", Object.assign(new Error("kill failed"), { code: "EPERM" }));
  h.clock.tick(60_000);
  assert.equal(h.children.length, 1);
  h.children[0].kill = () => {
    throw new Error("kill failed");
  };
  const stopping = h.service.stop();
  h.clock.tick(10_000);
  assert.match(h.service.state().error, /Could not stop/);
  h.children[0].emit("exit", 0, null);
  await stopping;
});

test("the built renderer has a standard CORS origin and cannot resolve outside its build", () => {
  assert.equal(APP_PRIVILEGES.standard, true);
  assert.equal(APP_PRIVILEGES.corsEnabled, true);
  assert.equal(APP_PRIVILEGES.bypassCSP, undefined);
  assert.equal(
    rendererFileUrl("rosetta://app/index.html?daemon=test", "/tmp/build"),
    "file:///tmp/build/index.html",
  );
  assert.equal(rendererFileUrl("rosetta://app/", "/tmp/build"), "file:///tmp/build/index.html");
  for (const url of [
    "rosetta://other/index.html",
    "file:///etc/passwd",
    "rosetta://app/..%2fprivate",
    "rosetta://app/%5c..%5csecret",
    "rosetta://app/%00",
    "rosetta://app/%zz",
  ]) {
    assert.equal(rendererFileUrl(url, "/tmp/build"), null, url);
  }
});
