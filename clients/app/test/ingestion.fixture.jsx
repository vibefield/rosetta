import { useState } from "react";
import { createRoot } from "react-dom/client";
import { HistoryRoom } from "../../room/src/HistoryRoom";
import { createDemoPorts, createRoomDemo } from "../../room/src/fixtures";
import "../renderer/src/styles/tokens.css";
import "../../room/src/styles/history-room.css";

const demo = createRoomDemo();
const base = createDemoPorts(demo);
const listeners = new Set();
const requests = [];
const error = `applying streamed ${"a".repeat(64)} sources: UNIQUE constraint failed: message.session_id, message.response_id, message.block_index`;
let run = {
  id: "ing_resume_test", index_id: "idx_resume_test", revision: 5,
  state: "failed", phase: "capturing", allowed_actions: ["resume"],
  progress_epoch: 1, error,
};
function update(state, allowed_actions, phase = "capturing") {
  run = { ...run, state, allowed_actions, phase, revision: run.revision + 1,
    error: state === "failed" ? error : null };
  for (const listener of listeners) listener();
}
const catalog = {
  ...base.catalog,
  subscribeMeta(handlers) {
    queueMicrotask(() => {
      handlers.onStatus?.("live");
      handlers.onChange?.({ runtimes: [], projects: [], sessions: [] }, "snapshot");
    });
    return { close() {} };
  },
  async status() {
    return { ...demo.status, canonicalReady: false, searchReadiness: "building",
      searchProgress: null, startedAt: Date.now() - 34_000, uptimeMs: 34_000,
      ingest: { ...demo.status.ingest, phase: "discovering", runtimes: [], run } };
  },
  onConnectionChange(listener) { listeners.add(listener); return () => listeners.delete(listener); },
};
function authority(label) {
  const command = (action, state, allowed) => async (id, command, signal) => {
    requests.push({ label, action, id, command, aborted: signal.aborted });
    update(state, allowed);
    return run;
  };
  return { resume: command("resume", "running", ["pause"]), pause: command("pause", "paused", ["resume"]) };
}
const primary = authority("primary"), replacement = authority("replacement");
function Fixture() {
  const [ingestion, setIngestion] = useState(primary);
  window.ingestionFixture = {
    requests,
    state: update,
    authority: (name) => setIngestion(name === "primary" ? primary : name === "replacement" ? replacement : null),
  };
  return <HistoryRoom {...base} catalog={catalog} ingestion={ingestion} storage={null} />;
}
createRoot(document.getElementById("root")).render(<Fixture />);
