// Production calendar, summary and dialog with deterministic daily activity.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { ProjectSummaryPanel } from "../renderer/src/components/SummaryPanels";
import { getActivityGridStartTimestamp, getCalendarDayKey } from "../renderer/src/lib/model";
import { NO_SPEND } from "../renderer/src/lib/spend";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/room.css";

const now = new Date(2026, 8, 30, 12).getTime();
const start = getActivityGridStartTimestamp(now);
const todayIndex = 364 + new Date(now).getDay();
const dailyTokens = Array.from({ length: 371 }, (_, index) => index > todayIndex ? -1
  : index === 279 || index === 298 || index === todayIndex - 11 ? 0
  : index >= 280 && index <= 297 ? 500000
  : index >= todayIndex - 10 ? (20 + index % 11) * 1700000
  : index > 337 ? (index % 7 === 0 ? 0 : (20 + index % 11) * 1700000)
  : index % 9 === 0 ? (1 + index % 7) * 1800000 : 0);
const today = getCalendarDayKey(now);
const usage = {
  hours: [{ hour: 3, tokens: 5000000 }, { hour: 16, tokens: 25000000 }, { hour: 17, tokens: 10000000 }],
  models: [
    { day: today, model: "claude-opus-5-5", tokens: 40800000 },
    { day: "2026-09-15", model: "claude-sonnet-4-5", tokens: 20000000 },
    { day: "2026-01-01", model: "older-model", tokens: 999999999 },
  ],
};

function Fixture() {
  const [mode, setMode] = useState("ready");
  const [projectId, setProjectId] = useState("demo");
  const values = mode === "zero" ? dailyTokens.map(value => value < 0 ? -1 : 0)
    : mode === "missing" ? dailyTokens.map(() => -1) : dailyTokens;
  const projectUsage = mode === "no-usage" ? null
    : mode === "zero" || mode === "missing" ? { hours: [], models: [] } : usage;
  const project = {
    row: { id: projectId, name: "vibe-field", root_paths: [], git_repo: "github.com/vibefield/rosetta" },
    tint: "#ce8be0", dailyTokens: values, currentDayIndex: todayIndex,
    rootSessionCount: 128, lifetimeMessages: 194529,
    tokensLast53Weeks: values.reduce((sum, value) => sum + Math.max(0, value), 0),
    spendLast53Weeks: { ...NO_SPEND, costMicroUsd: 28200000000 }, statsReady: mode !== "pending",
  };
  window.activityFixture = { setMode, switchProject: () => setProjectId("second"), today, todayIndex };
  return <main className="ah-room" style={{ minHeight: "100vh", padding: 20, background: "var(--vf-canvas-bg)" }}>
    <aside style={{ width: 310 }}><ProjectSummaryPanel project={project} visibleSessionCount={128}
      usage={projectUsage} activityGridStartTimestamp={start} currentTimestamp={now}
      ingestActive={false} censusRunning={false} isWorktreeVisible={true} onToggleWorktree={() => {}} /></aside>
  </main>;
}

createRoot(document.getElementById("root")).render(<Fixture />);
