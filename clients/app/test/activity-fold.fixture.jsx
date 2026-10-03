// Real document, synthetic canonical events, no daemon or user archive.
import { RoomProvider } from "../renderer/src/room-context";
import { ports } from "./support/room-ports.mjs";
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { TranscriptColumn } from "../renderer/src/components/Transcript";
import { Subthread } from "../renderer/src/components/transcript/Subthread";
import { ThreadNavigationContext } from "../renderer/src/components/transcript/thread-navigation";
import { buildThreadIndex } from "../renderer/src/components/transcript/thread-index";
import { entry, inline, shell } from "./support/ledger-fixtures.mjs";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/room.css";
import "../renderer/src/styles/transcript.css";

const nothing = () => {};
const now = Date.now() - 60_000;
const long = "# Pane layout report\n\nThe launcher belongs inside the existing pane.\n\n" +
  Array.from({ length: 40 }, (_, i) => `## Finding ${i + 1}\n\nKeep the original focus boundary and restore focus after the launcher closes. Saved sessions should reuse the existing launch request. Check keyboard navigation, Escape, long titles and narrow panes.`).join("\n\n") + "\n\nPEER REPORT END — preserve the complete result.";
function row(body, id, ordinal, group = "agent_cycle") {
  return entry(body, { timeline_id: id, ordinal, at: now + ordinal * 500,
    model: body.type === "agent" ? "claude-opus-5" : null,
    group: { id, kind: group, derivation: "native" } });
}
function chat(type, text, id, ordinal) {
  return row({ type, text: inline(text), attachments: [], citations: [] }, id, ordinal, type === "user" ? "human_turn" : "agent_cycle");
}
function peer(id, ordinal, text, extras = {}) {
  return row({ type: "collaboration", event: { kind: "message", channel: "peer", sender: "layout", recipient: "coordinator",
    team: null, color: null, summary: null, body: text === null ? null : inline(text), task_id: null,
    delivery_id: null, status: null, authored_at: null, source_session_id: null, source_response_id: null,
    work_start_timeline_id: null, work_end_timeline_id: null, ...extras } }, id, ordinal, "parent_invocation");
}
const initial = [
  peer("long-peer", 50, long),
  peer("team-message", 51, "Keep search above the saved sessions.", { channel: "team", color: "blue", recipient: "team" }),
  { ...shell("pnpm test pane-focus", { status: "failed", failure: { kind: "exit_non_zero", summary: "Escape did not restore focus", detail: null },
    timeline_id: "failed-tool", ordinal: 52, at: now + 26000 }) },
  peer("missing-peer", 53, null),
  row({ type: "summary", purpose: "compaction", text: inline("Continue with the existing pane frame.\n\n" + long) }, "recap", 54),
  peer("report", 55, "The documented focus path is correct.", { channel: "team", kind: "report", task_id: "task-1", summary: "Focus path reviewed" }),
  chat("user", "Build an empty-pane launcher and preserve keyboard focus.", "human", 56),
  chat("agent", "I’ll reuse the existing pane frame and session launch path.", "primary", 57),
  peer("short-peer", 58, "Also check Escape."),
  chat("agent", "The focus target is restored after the launcher closes.", "primary-two", 59),
  chat("user", "Run the final check.", "human-two", 60),
  chat("agent", "I’m checking it now.", "primary-three", 61),
  { ...shell("pnpm test pane-focus", { status: "running", timeline_id: "running-tool", ordinal: 62, at: now + 31000 }) },
  peer("tail-peer", 63, "The revised focus handler is ready."),
];
const session = { id: "s1", native_session_id: "activity-fixture", runtime_id: "claude", project_id: "p1",
  cwd: "/demo", parent_session_id: null, delegation: null, root_session_id: "s1", role: "main",
  title: "Empty-pane launcher", summary: null, started_at: now, updated_at: now + 60000,
  stream_state: "indexed", activity: { first_at: now, last_at: Date.now() } };

function Fixture() {
  const [rows, setRows] = useState(initial);
  const [focus, setFocus] = useState(null);
  const [currentSession, setSession] = useState(session);
  const [partial, setPartial] = useState(true);
  const [landed, setLanded] = useState(null);
  const [deep, setDeep] = useState(false);
  const child = { ...session, id: "deep-worker", parent_session_id: "worker-2", role: "subagent", title: "Keyboard worker" };
  window.activityFixture = {
    long,
    seek: id => setFocus({ sessionId: "s1", timelineId: id }),
    append: () => setRows(old => [...old, peer("new-peer", 64, "The final verification passed.")]),
    prepend: () => {
      setRows(old => [peer(old[0].ordinal === 50 ? "earlier-peer" : "older-peer", old[0].ordinal - 1, "Earlier notes from the same activity run."), ...old]);
      setPartial(false); setLanded(old => ({ seq: (old?.seq ?? 0) + 1, count: 1 }));
    },
    historical: () => setSession({ ...session, activity: { first_at: now - 86400000, last_at: now - 86400000 } }),
    reset: () => { setRows(initial); setPartial(true); setFocus(null); setSession(session); },
    clear: () => setSession(null),
    deep: () => setDeep(true),
  };
  return <div className="ah-frame" style={{ display: "flex", flexDirection: "column" }}>
    <div className="ah-room" style={{ position: "relative", inset: "auto", flex: 1, minHeight: 0 }}>
      <div className="ah-cols" style={{ display: "grid", gridTemplateColumns: "minmax(0, 1fr)" }}>
        <TranscriptColumn session={currentSession} sessions={[session]} duringBackfill={false}
          project={null} focus={focus} onFocusConsumed={() => setFocus(null)}
          feed={{ rows, snapshotReady: true, timelineMeta: null, status: "live", watermark: 61,
            earlier: { kind: partial ? "idle" : "none" }, earlierNotice: null, earlierAbove: partial ? 49 : 0, landed,
            holdEarlier: nothing, revealEarlier: nothing, retryEarlier: nothing, dismissEarlierNotice: nothing,
            canLoadEarlier: partial, loadEarlier: nothing, loadAnchor: async () => false }}
          onReceipt={nothing} stepper={null} onStepMatch={nothing} onDismissStepper={nothing}
          getRuntimeKind={() => "claude-code"} homeDirectory={null} />
      </div>
      {deep && <div id="deep-worker-fixture"><ThreadNavigationContext.Provider value={{ depth: 3, open: id => { window.openedThread = id; } }}>
        <Subthread session={child} anchored duringBackfill={false} index={buildThreadIndex([session, child])}
          focus={null} onFocusConsumed={nothing} onReceipt={nothing} onOpenAttachment={nothing}
          onOpenPath={nothing} getRuntimeKind={() => "claude-code"} homeDirectory={null} />
      </ThreadNavigationContext.Provider></div>}
    </div>
  </div>;
}
createRoot(document.getElementById("root")).render(<RoomProvider ports={ports}><Fixture /></RoomProvider>);
