import { strict as assert } from "node:assert";
import { test } from "node:test";

import {
  availableTimelineMetaKinds,
  resolveTimelineMetaKind,
} from "../renderer/src/components/transcript/timeline-meta-state.ts";

const task = {
  id: "task-1",
  subject: "Keep the menu honest",
  status: "pending",
  started_at: null,
  terminal_at: null,
  timeline_id: "row-1",
};
const artifact = {
  id: "artifact-1",
  name: "result.txt",
  artifact_kind: "file",
  created_at: 1,
  byte_len: 4,
  media_type: "text/plain",
  source: { type: "timeline_file", timeline_id: "row-2", target_index: 0, path: "result.txt" },
};

test("timeline_meta exposes only categories with items", () => {
  assert.deepEqual(
    availableTimelineMetaKinds({ tasks: [task], plans: [], artifacts: [artifact] }),
    ["tasks", "artifacts"],
  );
  assert.deepEqual(
    availableTimelineMetaKinds({ tasks: [], plans: [], artifacts: [] }),
    [],
  );
});

test("a removed active category falls through to the first remaining one", () => {
  assert.equal(resolveTimelineMetaKind("plans", ["tasks", "artifacts"]), "tasks");
  assert.equal(resolveTimelineMetaKind("plans", []), null);
  assert.equal(resolveTimelineMetaKind(null, ["tasks"]), null);
});
