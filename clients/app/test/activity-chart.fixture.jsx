// Production calendar, summary and dialog with deterministic daily activity.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { ProjectSummaryPanel } from "../../room/src/components/SummaryPanels";
import { getActivityGridStartTimestamp, getCalendarDayKey } from "../../room/src/lib/model";
import { RoomProvider } from "../../room/src/room-context";
import { client, ports } from "./support/room-ports.mjs";
import { NO_SPEND } from "../../room/src/lib/spend";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/app.css";
import "../../room/src/styles/room.css";

const now = new Date(2026, 8, 30, 23, 45).getTime();
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

const requests = [];
client.dayUsage = async (projectId, day, offsetMinutes, signal) => {
  const mode = window.activityFixture.mode;
  const recordedHour = window.activityFixture.hour;
  requests.push({ projectId, day, offsetMinutes, signal });
  // Deliberately let a slow request resolve after cancellation to exercise
  // the production hook's stale-result guard, not only AbortController.
  await new Promise(resolve => setTimeout(resolve, day === "2026-09-23" ? 500 : 110));
  if (mode === "error") throw new Error("Fixture API unavailable");
  const dayIndex = Math.round((new Date(`${day}T12:00:00`).getTime() - start) / 86400000 - .5);
  const total = mode === "zero" || mode === "no-usage" ? 0 : Math.max(0, dailyTokens[dayIndex] ?? 0);
  const models = [];
  const patterns = [
    [0,0,0,0,0,0,0,0,0,2,5,2,0,1,3,7,10,4,0,2,1,0,0,0],
    [0,0,0,0,0,0,0,0,0,0,1,3,1,0,0,0,2,5,4,1,0,0,0,0],
    [0,0,0,0,0,0,0,0,1,2,0,0,0,0,0,2,0,0,0,0,0,0,0,0],
  ];
  ["claude-opus-5-5", "claude-sonnet-4-5", "gpt-5.4"].forEach((model, index) => {
    let remaining = Math.round(total * [.6, .3, .1][index]);
    const weights = patterns[index].map((weight, hour) => day === today && hour > recordedHour ? 0 : weight);
    const weightTotal = weights.reduce((sum, value) => sum + value, 0);
    const last = weights.findLastIndex(weight => weight > 0);
    weights.forEach((weight, hour) => {
      if (!weight) return;
      const tokens = hour === last ? remaining : Math.floor(total * [.6, .3, .1][index] * weight / weightTotal);
      remaining -= tokens;
      if (tokens > 0) models.push({ hour, model, tokens });
    });
  });
  return { day, models };
};

function Fixture() {
  const [mode, setMode] = useState("ready");
  const [hour, setHour] = useState(23);
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
  window.activityFixture = { setMode, mode, requests, setHour, hour, switchProject: () => setProjectId("second"), today, todayIndex };
  return <main className="ah-room" style={{ minHeight: "100vh", padding: 20, background: "var(--vf-canvas-bg)" }}>
    <aside style={{ width: 310 }}><ProjectSummaryPanel project={project} visibleSessionCount={128}
      usage={projectUsage} activityGridStartTimestamp={start} currentTimestamp={new Date(now).setHours(hour, 45)}
      ingestActive={false} censusRunning={false} isWorktreeVisible={true} onToggleWorktree={() => {}} /></aside>
  </main>;
}

createRoot(document.getElementById("root")).render(<RoomProvider ports={ports}><Fixture /></RoomProvider>);
