import { strict as assert } from "node:assert";
import { test } from "node:test";
import { RosettaClient } from "../../ts/client.ts";

class FakeSocket {
  static instances = [];

  constructor(url) {
    this.url = url;
    FakeSocket.instances.push(this);
  }

  open() {
    this.onopen?.({});
  }

  frame(value) {
    this.onmessage?.({ data: JSON.stringify(value) });
  }

  close() {
    this.onclose?.({});
  }
}

const EMPTY_META = { tasks: [], workflows: [], plans: [], artifacts: [] };

test("a timeline_meta replacement updates only the light work index", () => {
  FakeSocket.instances = [];
  const rows = [];
  const metadata = [];
  const statuses = [];
  const client = new RosettaClient({
    baseUrl: "http://127.0.0.1:4640",
    WebSocketImpl: FakeSocket,
    fetchFn: () => {
      throw new Error("the metadata stream must not make an HTTP request");
    },
  });
  const subscription = client.subscribeSession("session/one", {
    onChange: (value) => rows.push(value),
    onTimelineMetaChange: (value) => metadata.push(value),
    onStatus: (value) => statuses.push(value),
  });
  const socket = FakeSocket.instances[0];
  assert.equal(socket.url, "ws://127.0.0.1:4640/ws/sessions/session%2Fone");

  socket.open();
  socket.frame({
    frame: "hello",
    protocol_version: 10,
    stamp: { epoch: 4, seq: 8 },
  });
  socket.frame({
    frame: "timeline_snapshot",
    stamp: { epoch: 4, seq: 8 },
    entries: [],
    // A rolling restart can briefly pair this client with a daemon from
    // before the additive Workflow index existed.
    timeline_meta: { tasks: [], plans: [], artifacts: [] },
  });
  socket.frame({
    frame: "timeline_meta_snapshot",
    stamp: { epoch: 4, seq: 9 },
    timeline_meta: {
      tasks: [{
        id: "task-1",
        subject: "Ship the index",
        status: "in_progress",
        started_at: 1000,
        terminal_at: null,
        timeline_id: "message-1",
      }],
      workflows: [{
        id: "workflow-1",
        name: "Review in parallel",
        status: "running",
        started_at: 2_000,
        duration_ms: null,
        worker_count: 3,
        timeline_id: "message-2",
      }],
      plans: [],
      artifacts: [],
    },
  });

  assert.equal(rows.length, 1, "metadata replacement does not redraw the message stream");
  assert.deepEqual(metadata[0], EMPTY_META);
  assert.equal(metadata[1].tasks[0].subject, "Ship the index");
  assert.equal(metadata[1].workflows[0].name, "Review in parallel");
  assert.equal(statuses.at(-1), "live");
  subscription.close();
  assert.equal(statuses.at(-1), "closed");
});

test("a live workflow update preserves the read-hydrated run document", () => {
  FakeSocket.instances = [];
  const changes = [];
  const client = new RosettaClient({
    baseUrl: "http://127.0.0.1:4640",
    WebSocketImpl: FakeSocket,
    fetchFn: () => {
      throw new Error("the timeline stream must not make an HTTP request");
    },
  });
  const action = {
    type: "workflow",
    name: "api-design",
    path: null,
    arguments: null,
    objective: null,
    resume_id: null,
    background: true,
    task_id: "task-1",
    run_id: "wf-1",
    attempt: 1,
    callback_summary: null,
    callback_result: null,
    usage: null,
    run: { run_id: "wf-1", status: "running", attempts: [], journal: [] },
  };
  const tool = {
    correlation: { key: "tool-1", derivation: "native_id" },
    native: { id: "tool-1", namespace: null, name: "Workflow", provider: null },
    status: { value: "running", basis: "ground_truth_protocol" },
    action,
    failure: null,
    timing: null,
  };
  const row = {
    timeline_id: "workflow-row",
    session_id: "session/one",
    ordinal: 1,
    group: { id: "group-1", kind: "agent_cycle", derivation: "native" },
    response: null,
    block_index: null,
    native_id: null,
    parent_native_id: null,
    at: 1_000,
    model: null,
    effort: null,
    body: { type: "tool_use", tool },
    parent_tool_native_id: null,
    usage: { input: null, output: null, cache_read: null, cache_write: null, thinking: null },
    provenance: { agent_version: null, src_cursor: null, src_offset: null },
  };
  const subscription = client.subscribeSession("session/one", {
    onChange: (value) => changes.push(value),
  });
  const socket = FakeSocket.instances[0];
  socket.open();
  socket.frame({ frame: "hello", protocol_version: 14, stamp: { epoch: 1, seq: 1 } });
  socket.frame({
    frame: "timeline_snapshot",
    stamp: { epoch: 1, seq: 1 },
    entries: [row],
    timeline_meta: EMPTY_META,
  });
  socket.frame({
    frame: "mutation",
    stamp: { epoch: 1, seq: 2 },
    mutation: {
      type: "tool_updated",
      session_id: "session/one",
      timeline_id: "workflow-row",
      tool: {
        ...tool,
        status: { value: "succeeded", basis: "ground_truth_protocol" },
        action: { ...action, callback_summary: "completed", run: null },
      },
      at: 2_000,
    },
  });

  const updated = changes.at(-1)[0].body.tool;
  assert.equal(updated.status.value, "succeeded");
  assert.equal(updated.action.callback_summary, "completed");
  assert.equal(updated.action.run.run_id, "wf-1");

  socket.frame({
    frame: "mutation",
    stamp: { epoch: 1, seq: 3 },
    mutation: {
      type: "message",
      ...row,
      at: 3_000,
      body: {
        type: "tool_use",
        tool: {
          ...tool,
          status: { value: "failed", basis: "ground_truth_protocol" },
          action: { ...action, callback_summary: "replacement", run: null },
        },
      },
    },
  });

  const replaced = changes.at(-1)[0].body.tool;
  assert.equal(replaced.status.value, "failed");
  assert.equal(replaced.action.callback_summary, "replacement");
  assert.equal(replaced.action.run.run_id, "wf-1");
  subscription.close();
});

test("oversized canonical text is read from its daemon asset", async () => {
  let requested = null;
  let requestInit = null;
  const client = new RosettaClient({
    baseUrl: "http://127.0.0.1:4640",
    WebSocketImpl: FakeSocket,
    fetchFn: async (input, init) => {
      requested = String(input);
      requestInit = init;
      return new Response("the complete workflow result");
    },
  });
  const text = await client.readTextAsset({
    url: "/api/assets/abc.txt",
    digest: "a".repeat(64),
    media_type: "text/plain; charset=utf-8",
    byte_len: 28,
  });
  assert.equal(text, "the complete workflow result");
  assert.equal(requested, "http://127.0.0.1:4640/api/assets/abc.txt");
  assert.equal(requestInit.redirect, "error");
  await assert.rejects(
    client.readTextAsset({
      url: "https://example.com/result.txt",
      digest: "b".repeat(64),
      media_type: "text/plain; charset=utf-8",
      byte_len: 1,
    }),
    /unsupported address/,
  );
});
