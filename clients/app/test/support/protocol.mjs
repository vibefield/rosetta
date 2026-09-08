import { RosettaClient, PROTOCOL_VERSION } from "../../../ts/client.ts";

export const EMPTY_META = { tasks: [], workflows: [], plans: [], artifacts: [] };
export const row = (
  id,
  ordinal = 1,
  body = { type: "agent", text: { type: "inline", text: id } },
) => ({
  timeline_id: id,
  session_id: "s1",
  ordinal,
  body,
  at: ordinal,
  model: null,
  effort: null,
  response: null,
  usage: {},
});
export function response(body, version = PROTOCOL_VERSION) {
  return new Response(JSON.stringify(body), {
    headers: { "x-rosetta-protocol-version": String(version) },
  });
}
export function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
export function fakeClock(t) {
  let now = 0;
  let next = 0;
  const pending = new Map();
  const originalSet = globalThis.setTimeout;
  const originalClear = globalThis.clearTimeout;
  globalThis.setTimeout = (fn, delay = 0) => {
    const id = ++next;
    pending.set(id, { fn, at: now + delay });
    return id;
  };
  globalThis.clearTimeout = (id) => pending.delete(id);
  t.after(() => {
    globalThis.setTimeout = originalSet;
    globalThis.clearTimeout = originalClear;
  });
  return {
    tick(ms) {
      const until = now + ms;
      while (true) {
        const item = [...pending]
          .filter(([, v]) => v.at <= until)
          .sort((a, b) => a[1].at - b[1].at)[0];
        if (!item) break;
        pending.delete(item[0]);
        now = item[1].at;
        item[1].fn();
      }
      now = until;
    },
  };
}
export function harness(t, options = {}, handlers = {}) {
  const sockets = [];
  class Socket {
    constructor(url) {
      this.url = url;
      this.closed = false;
      sockets.push(this);
    }
    open(stamp = { epoch: 1, seq: 0 }, version = PROTOCOL_VERSION) {
      this.onopen?.({});
      this.frame({ frame: "hello", stamp, protocol_version: version });
    }
    frame(frame) {
      this.onmessage?.({ data: JSON.stringify(frame) });
    }
    close() {
      this.closed = true;
      this.onclose?.({});
    }
    snapshot(entries = [], seq = 10, cursor = "older", watermark = "generation-a") {
      this.frame({
        frame: "timeline_snapshot",
        entries,
        stamp: { epoch: 1, seq },
        timeline_meta: EMPTY_META,
        cursor,
        watermark,
      });
    }
  }
  const changes = [];
  const statuses = [];
  const errors = [];
  const client = new RosettaClient({
    WebSocketImpl: Socket,
    notificationIntervalMs: 0,
    ...options,
  });
  const sub = client.subscribeSession("s1", {
    onChange: (entries) => changes.push(entries),
    onStatus: (status) => statuses.push(status),
    onError: (error) => errors.push(error),
    ...handlers,
  });
  t.after(() => sub.close());
  return { client, sub, sockets, changes, statuses, errors, socket: sockets[0] };
}
