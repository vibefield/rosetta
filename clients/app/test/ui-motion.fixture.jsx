// Real production components with deterministic data; no daemon or user stores.
import { RoomProvider } from "../renderer/src/room-context";
import { ports } from "./support/room-ports.mjs";
import { useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { ProjectList } from "../renderer/src/components/projects/ProjectList";
import { SessionList } from "../renderer/src/components/sessions/SessionList";
import { TranscriptColumn } from "../renderer/src/components/Transcript";
import { NO_SPEND } from "../renderer/src/lib/spend";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/app.css";
import "../renderer/src/styles/room.css";
import "../renderer/src/styles/transcript.css";

const now = new Date(2026, 8, 7, 14).getTime();
const nothing = () => {};
const projects = ["Rosetta", "Orchard", "Field Notes", "Compass", "Mille", "Daylight"].map(
  (name, i) => ({
    row: {
      id: `p${i}`,
      name,
      root_paths: [`/Users/demo/Projects/${name.toLowerCase().replaceAll(" ", "-")}`],
      git_repo: null,
    },
    tint: ["#6366f1", "#22c55e", "#06b6d4", "#f59e0b", "#ec4899", "#8b5cf6"][i],
    lifetimeTokens: 345000 - i * 37000,
    lifetimeSpend: { ...NO_SPEND, costMicroUsd: 3450000 - i * 370000 },
    spendLast30Days: NO_SPEND,
    spendLast53Weeks: NO_SPEND,
    tokensLast30Days: 30000,
    recentTokenBuckets: Array.from({ length: 15 }, (_, n) => 100 + ((n * 17 + i * 13) % 43) * 30),
    dailyTokens: [],
    currentDayIndex: 0,
    tokensLast53Weeks: 300000,
    lifetimeMessages: 42,
    agents: [],
    lastActiveMs: now - i * 180000,
    rootSessionCount: 12,
    sessionCount: 12,
    indexedSessionCount: 12,
    readiness: "ready",
    statsReady: true,
  }),
);
const sessions = [
  "Polish the reading experience",
  "Review daemon communication",
  "Refine search results",
  "Keyboard navigation",
  "Build the activity view",
  "Improve timeline loading",
  "Add project summaries",
  "Review the typography",
].map((title, i) => ({
  id: `s${i}`,
  native_session_id: `session-${String(i + 1).padStart(4, "0")}`,
  runtime_id: "claude",
  project_id: "p0",
  cwd: "/Users/demo/Projects/rosetta",
  parent_session_id: null,
  delegation: null,
  root_session_id: `s${i}`,
  role: "main",
  title,
  summary: null,
  started_at: now - i * 3_600_000 - 600000,
  updated_at: now, // rescan rewrote metadata; conversation dates below stay old
  activity: {
    first_at: now - (i < 4 ? i * 3600000 : 86400000 + i * 3600000) - 600000,
    last_at: now - (i < 4 ? i * 3600000 : 86400000 + i * 3600000),
  },
  stream_state: i === 0 ? "streaming" : "indexed",
}));
const prompts = [
  "Make the project and session lists feel fluid.",
  "Keep my place while new messages arrive.",
  "Give me a clear way back to the latest message.",
];
const replies = [
  "The list keeps each row’s identity as its position changes. A short transition carries it to its new place, so your eye can follow the movement.",
  "New messages settle into the conversation gently. When you scroll up to read, the transcript holds your position and quietly counts new arrivals.",
  "The latest-message control stays within reach. It brings you smoothly back to the conversation, and you can interrupt the movement at any point.",
];
function message(ordinal) {
  const human = ordinal % 2 === 0;
  return {
    timeline_id: `t${ordinal}`,
    session_id: "s0",
    ordinal,
    group: {
      id: `g${Math.floor(ordinal / 2)}`,
      kind: human ? "human_turn" : "agent_cycle",
      derivation: "native",
    },
    response: human ? null : { id: `r${ordinal}`, derivation: "native" },
    block_index: null,
    native_id: null,
    parent_native_id: null,
    at: now - 300000 + ordinal * 1000,
    model: human ? null : "claude-sonnet-4",
    effort: null,
    body: {
      type: human ? "user" : "agent",
      text: { type: "inline", text: (human ? prompts : replies)[Math.floor(ordinal / 2) % 3] },
      attachments: [],
      citations: [],
    },
    parent_tool_native_id: null,
    usage: { input: null, output: null, cache_read: null, cache_write: null, thinking: null },
    provenance: { agent_version: null, src_cursor: null, src_offset: null },
  };
}

function Fixture() {
  const [projectRows, setProjects] = useState(projects);
  const [sessionRows, setSessions] = useState(sessions);
  const [rows, setRows] = useState(() => Array.from({ length: 24 }, (_, i) => message(i + 100)));
  const [selectedId, setSelectedId] = useState("s0");
  const [focus, setFocus] = useState(null);
  const [landed, setLanded] = useState(null);
  const [ready, setReady] = useState(true);
  const activeSession = sessionRows.find((row) => row.id === selectedId);
  const displayedRows = useMemo(
    () => rows.map((row) => ({ ...row, session_id: selectedId })),
    [rows, selectedId],
  );
  const feed = {
    rows: displayedRows,
    snapshotReady: ready,
    timelineMeta: {
      tasks: [
        { id: "stale-task", subject: "Unfinished archived task", status: "in_progress", started_at: now - 30 * 86400000, terminal_at: null, timeline_id: null },
        { id: "finished-task", subject: "Finished archived task", status: "completed", started_at: now - 60000, terminal_at: now, timeline_id: null },
      ],
      workflows: [
        { id: "stale-workflow", name: "Unfinished archived workflow", status: "running", started_at: now - 30 * 86400000, duration_ms: null, worker_count: 0, child_session_ids: [], timeline_id: null },
      ],
      plans: [], artifacts: [],
    },
    status: "live",
    watermark: 123,
    earlier: { kind: "none" },
    earlierNotice: null,
    earlierAbove: 0,
    landed,
    holdEarlier: nothing,
    revealEarlier: nothing,
    retryEarlier: nothing,
    dismissEarlierNotice: nothing,
    canLoadEarlier: false,
    loadEarlier: nothing,
    loadAnchor: async () => false,
  };
  const append = (count = 1) =>
    setRows((old) => [
      ...old,
      ...Array.from({ length: count }, (_, i) => message(old.at(-1).ordinal + 1 + i)),
    ]);
  const grow = () =>
    setRows((old) =>
      old.map((row, i) =>
        i === old.length - 1
          ? {
              ...row,
              body: {
                ...row.body,
                text: { type: "inline", text: row.body.text.text + "\n\n" + replies.join("\n\n") },
              },
            }
          : row,
      ),
    );
  const prepend = () => {
    setRows((old) => [
      ...Array.from({ length: 10 }, (_, i) => message(old[0].ordinal - 10 + i)),
      ...old,
    ]);
    setLanded((old) => ({ seq: (old?.seq ?? 0) + 1, count: 10 }));
  };
  const promote = () =>
    setSessions((old) => {
      const last = old.at(-1);
      return [{ ...last, activity: { ...last.activity, last_at: now } }, ...old.slice(0, -1)];
    });
  // The integration runner uses the same state transitions as these controls.
  Object.assign(window, {
    motionFixture: {
      append,
      grow,
      prepend,
      promote,
      flush: flushSync,
      reorder: () => setProjects((old) => [...old].reverse()),
      switchSession: () => setSelectedId((old) => (old === "s0" ? "s1" : "s0")),
      seek: () => setFocus({ sessionId: selectedId, timelineId: rows[4].timeline_id }),
      ready: setReady,
    },
  });
  return (
    <div style={{ height: "100%", display: "flex", flexDirection: "column" }}>
      <div
        style={{
          padding: "12px 18px",
          display: "flex",
          gap: 12,
          borderBottom: "1px solid var(--vf-hairline-strong)",
        }}
      >
        <strong style={{ marginRight: "auto", fontSize: 13 }}>Rosetta · Motion preview</strong>
        <button onClick={() => window.motionFixture.reorder()}>Reorder projects</button>
        <button onClick={promote}>Promote session</button>
        <button onClick={() => append()}>Append message</button>
        <button onClick={() => append(5)}>Burst</button>
        <button onClick={grow}>Grow reply</button>
        <button onClick={prepend}>Earlier history</button>
      </div>
      <div
        className="ah-room"
        style={{
          position: "relative",
          inset: "auto",
          display: "grid",
          gridTemplateColumns: "240px 280px minmax(0, 1fr)",
          flex: 1,
          minHeight: 0,
        }}
      >
        <div className="ah-col">
          <header>
            <span className="ah-eyebrow">Projects</span>
          </header>
          <ProjectList
            projects={projectRows}
            selectedProjectId="p0"
            maximumLifetimeCost={3450000}
            homeDirectory="/Users/demo"
            currentTimestamp={now}
            ingestActive={false}
            projectBackfills={new Map()}
            pending={false}
            onProjectSelect={nothing}
          />
        </div>
        <div className="ah-col">
          <header>
            <span className="ah-eyebrow">Sessions</span>
          </header>
          <SessionList
            sessions={sessionRows}
            factsBySessionId={new Map()}
            getRuntimeKind={() => "claude-code"}
            selectedSessionId={selectedId}
            currentTimestamp={now}
            duringBackfill={false}
            pending={false}
            onSessionSelect={(session) => setSelectedId(session.id)}
          />
        </div>
        <TranscriptColumn
          session={activeSession}
          sessions={sessionRows}
          project={projects[0]}
          duringBackfill={false}
          feed={feed}
          focus={focus}
          onFocusConsumed={() => setFocus(null)}
          onReceipt={nothing}
          stepper={null}
          onStepMatch={nothing}
          onDismissStepper={nothing}
          getRuntimeKind={() => "claude-code"}
          homeDirectory="/Users/demo"
        />
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")).render(<RoomProvider ports={ports}><Fixture /></RoomProvider>);
