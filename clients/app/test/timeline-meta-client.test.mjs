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

const EMPTY_META = { tasks: [], plans: [], artifacts: [] };

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
    timeline_meta: EMPTY_META,
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
      plans: [],
      artifacts: [],
    },
  });

  assert.equal(rows.length, 1, "metadata replacement does not redraw the message stream");
  assert.deepEqual(metadata[0], EMPTY_META);
  assert.equal(metadata[1].tasks[0].subject, "Ship the index");
  assert.equal(statuses.at(-1), "live");
  subscription.close();
  assert.equal(statuses.at(-1), "closed");
});
