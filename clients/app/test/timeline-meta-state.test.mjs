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
const workflow = {
  id: "workflow-1",
  name: "Review in parallel",
  status: "running",
  started_at: 1,
  duration_ms: null,
  worker_count: 3,
  timeline_id: "row-3",
};

test("timeline_meta exposes only categories with items", () => {
  assert.deepEqual(
    availableTimelineMetaKinds({
      tasks: [task],
      workflows: [workflow],
      plans: [],
      artifacts: [artifact],
    }),
    ["tasks", "workflows", "artifacts"],
  );
  assert.deepEqual(
    availableTimelineMetaKinds({ tasks: [], workflows: [workflow], plans: [], artifacts: [] }),
    ["workflows"],
  );
  assert.deepEqual(
    availableTimelineMetaKinds({ tasks: [], workflows: [], plans: [], artifacts: [] }),
    [],
  );
  assert.deepEqual(
    availableTimelineMetaKinds({ tasks: [], plans: [], artifacts: [] }),
    [],
  );
});

test("a removed active category falls through to the first remaining one", () => {
  assert.equal(resolveTimelineMetaKind("plans", ["tasks", "artifacts"]), "tasks");
  assert.equal(resolveTimelineMetaKind("plans", ["workflows"]), "workflows");
  assert.equal(resolveTimelineMetaKind("plans", []), null);
  assert.equal(resolveTimelineMetaKind(null, ["tasks"]), null);
});
