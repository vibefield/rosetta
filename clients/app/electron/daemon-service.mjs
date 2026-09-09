// Own exactly one daemon generation. Readiness is structured, failed children
// are reaped before retry, and shutdown awaits the actual child exit.
import { spawn } from "node:child_process";
import { mkdirSync } from "node:fs";
import path from "node:path";
import { PROTOCOL_VERSION } from "../../ts/protocol-version.mjs";

export function createDaemonService({
  bin,
  dbPath,
  onState = () => {},
  spawnProcess = spawn,
  startupTimeoutMs = 8000,
  shutdownTimeoutMs = 10_000,
  restartBaseMs = 500,
  restartMaxMs = 10_000,
  stableAfterMs = 30_000,
  stdout = (chunk) => process.stdout.write(`[rosettad] ${chunk}`),
  stderr = (chunk) => process.stderr.write(`[rosettad] ${chunk}`),
}) {
  let current = null;
  let retryTimer = null;
  let stopped = false;
  let attempts = 0;
  let state = { phase: "starting", origin: null, error: null };
  const publish = (phase, origin = null, error = null) => {
    state = { phase, origin, error };
    onState(state);
  };
  const retry = () => {
    if (stopped || retryTimer !== null) return;
    const delay = Math.min(restartMaxMs, restartBaseMs * 2 ** attempts++);
    retryTimer = setTimeout(() => {
      retryTimer = null;
      void launch();
    }, delay);
  };
  const signal = (generation, name) => {
    try {
      generation.proc.kill(name);
    } catch (error) {
      // A failed signal is not evidence that the process exited. Keep owning
      // this generation until the OS reports its exit; never overlap children.
      publish("failed", null, `Could not stop the daemon: ${error.message}`);
    }
  };
  const terminate = (generation) => {
    if (generation.exited || generation.terminating) return generation.exit;
    generation.terminating = true;
    clearTimeout(generation.startupTimer);
    clearTimeout(generation.stableTimer);
    generation.killTimer = setTimeout(() => {
      if (!generation.exited) signal(generation, "SIGKILL");
    }, shutdownTimeoutMs);
    signal(generation, "SIGINT");
    return generation.exit;
  };
  function launch() {
    if (stopped) return Promise.resolve(null);
    if (current !== null) return current.ready;
    publish(attempts === 0 ? "starting" : "restarting");
    let proc;
    try {
      mkdirSync(path.dirname(dbPath), { recursive: true });
      // `--announce` publishes the ephemeral origin for sibling apps (Godview);
      // the stdout ready line stays the app's own channel.
      proc = spawnProcess(bin, ["serve", "--db", dbPath, "--addr", "127.0.0.1:0", "--json", "--announce"], {
        stdio: ["ignore", "pipe", "pipe"],
      });
    } catch (error) {
      publish("failed", null, error.message);
      return Promise.resolve(null);
    }
    let resolveReady;
    let resolveExit;
    const generation = {
      proc,
      exited: false,
      settled: false,
      terminating: false,
      terminal: false,
      greeting: "",
      startupTimer: null,
      stableTimer: null,
      killTimer: null,
      ready: new Promise((resolve) => {
        resolveReady = resolve;
      }),
      exit: new Promise((resolve) => {
        resolveExit = resolve;
      }),
    };
    current = generation;
    const settle = (origin) => {
      if (generation.settled) return;
      generation.settled = true;
      resolveReady(origin);
    };
    const fail = (message, terminal = false) => {
      if (current !== generation || generation.exited || generation.terminating) return;
      generation.terminal = terminal;
      publish(terminal ? "failed" : "restarting", null, message);
      settle(null);
      void terminate(generation);
    };
    const exited = (message) => {
      if (generation.exited) return;
      generation.exited = true;
      clearTimeout(generation.startupTimer);
      clearTimeout(generation.stableTimer);
      clearTimeout(generation.killTimer);
      settle(null);
      resolveExit();
      if (current !== generation) return;
      current = null;
      if (stopped) return;
      if (!generation.terminal) {
        publish("restarting", null, message);
        retry();
      }
    };
    proc.stdout.on("data", (chunk) => {
      stdout(chunk);
      if (current !== generation || generation.settled || generation.terminating || stopped) return;
      generation.greeting += String(chunk);
      let end;
      while ((end = generation.greeting.indexOf("\n")) !== -1) {
        const line = generation.greeting.slice(0, end);
        generation.greeting = generation.greeting.slice(end + 1);
        let event;
        try {
          event = JSON.parse(line);
        } catch {
          continue;
        }
        if (event?.event !== "ready") continue;
        if (event.protocol_version !== PROTOCOL_VERSION) {
          fail(
            `Daemon protocol ${event.protocol_version} does not match this app (${PROTOCOL_VERSION}).`,
            true,
          );
          return;
        }
        let url;
        try {
          url = new URL(event.origin);
        } catch {
          fail("The daemon reported an invalid origin.", true);
          return;
        }
        if (
          url.protocol !== "http:" ||
          url.hostname !== "127.0.0.1" ||
          !url.port ||
          url.port === "0" ||
          url.username ||
          url.password ||
          url.pathname !== "/" ||
          url.search ||
          url.hash
        ) {
          fail("The daemon reported an invalid loopback origin.", true);
          return;
        }
        clearTimeout(generation.startupTimer);
        publish("ready", url.origin);
        settle(url.origin);
        generation.stableTimer = setTimeout(() => {
          if (current === generation) attempts = 0;
        }, stableAfterMs);
        return;
      }
      if (generation.greeting.length > 64 * 1024) {
        fail("The daemon readiness message exceeded its limit.", true);
      }
    });
    proc.stderr.on("data", stderr);
    proc.on("exit", (code, signal) => exited(`Daemon exited (${signal ?? code}).`));
    proc.on("error", (error) => {
      if (proc.pid === undefined) {
        // Failed spawns have no process and do not emit an exit event.
        generation.terminal = ["ENOENT", "EACCES"].includes(error.code);
        if (generation.terminal) publish("failed", null, error.message);
        exited(error.message);
      } else {
        publish("failed", null, error.message);
      }
    });
    generation.startupTimer = setTimeout(
      () => fail("The daemon did not become ready in time."),
      startupTimeoutMs,
    );
    return generation.ready;
  }
  return {
    start: launch,
    state: () => state,
    async stop() {
      stopped = true;
      clearTimeout(retryTimer);
      retryTimer = null;
      if (current !== null) await terminate(current);
      publish("stopped");
    },
  };
}
