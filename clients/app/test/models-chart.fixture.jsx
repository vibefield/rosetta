import { useState } from "react";
import { createRoot } from "react-dom/client";
import { ProjectSummaryPanel } from "../../room/src/components/SummaryPanels";
import { getActivityGridStartTimestamp } from "../../room/src/lib/model";
import { RoomProvider } from "../../room/src/room-context";
import { client, ports } from "./support/room-ports.mjs";
import { NO_SPEND } from "../../room/src/lib/spend";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/app.css";
import "../../room/src/styles/room.css";

const now = new Date(2026, 8, 30, 17, 35).getTime();
const start = getActivityGridStartTimestamp(now);
const todayIndex = 364 + new Date(now).getDay();
const ids = ["claude-opus-5-5", "claude-fable-5-1", "claude-opus-5", "claude-sonnet-4-5",
  "gpt-5.4", "gpt-5.3-codex", "gemini-3.1-pro", "qwen3.5-coder", "deepseek-v4"];
const rows = [];
const days = Array.from({ length: 30 }, (_, index) => `2026-09-${String(index + 1).padStart(2, "0")}`);
for (let index = 0; index < 30; index++) {
  const scale = index === 5 ? 0 : (index > 20 ? 4 + Math.sin(index * .8) * 2 : 1.2 + Math.cos(index * .7) * .7) * 1e8;
  ids.forEach((model, modelIndex) => {
    const trend = modelIndex === 0 ? index > 17 ? 2.8 : .4 : modelIndex === 1 ? 1.4 : modelIndex === 2 ? index < 15 ? 1.2 : .2 : .04;
    const tokens = Math.round(scale * trend * (.65 + Math.sin(index * .8 + modelIndex * 1.9) ** 2));
    if (tokens > 0) rows.push({ day: days[index], model, tokens });
  });
}
const dailyTotals = days.map(day => rows.filter(row => row.day === day).reduce((sum, row) => sum + row.tokens, 0));
const usage = { models: [...rows, { day: "2026-08-01", model: "legacy-model", tokens: 99999999999 }], hours: [] };
const extraRows = Array.from({ length: 24 }, (_, index) => ({
  day: "2026-09-30", model: `fixture-model-${String(index + 1).padStart(2, "0")}`, tokens: (index + 1) * 10000,
}));
const manyUsage = { ...usage, models: [...usage.models, ...extraRows] };
const requests = [];
client.dayUsage = async (projectId, day, offsetMinutes, signal) => {
  requests.push({ projectId, day, offsetMinutes, signal });
  return { day, models: rows.filter(row => row.day === day).map(row => ({ hour: 12, model: row.model, tokens: row.tokens })) };
};

function Fixture() {
  const [mode, setMode] = useState("ready");
  const [projectId, setProjectId] = useState("demo");
  const [name, setName] = useState("vibe-field");
  const activeRows = mode === "many" ? [...rows, ...extraRows] : rows;
  const activeIds = mode === "many" ? [...ids, ...extraRows.map(row => row.model)] : ids;
  const activeTotals = mode === "many" ? days.map(day => activeRows.filter(row => row.day === day).reduce((sum, row) => sum + row.tokens, 0)) : dailyTotals;
  const dailyTokens = Array.from({ length: 371 }, (_, index) => index > todayIndex ? -1
    : index >= todayIndex - 29 ? activeTotals[index - (todayIndex - 29)] : 0);
  window.modelsFixture = { ids: activeIds, days, rows: activeRows, dailyTotals: activeTotals, requests, mode, setMode, setName, switchProject: () => setProjectId("second") };
  const project = {
    row: { id: projectId, name, root_paths: [], git_repo: "github.com/vibefield/rosetta" },
    tint: "#ce8be0", dailyTokens, currentDayIndex: todayIndex, rootSessionCount: 128,
    lifetimeMessages: 194529, tokensLast53Weeks: activeTotals.reduce((sum, value) => sum + value, 0),
    spendLast53Weeks: NO_SPEND, statsReady: mode !== "pending",
  };
  const projectUsage = mode === "unavailable" ? null : mode === "empty" ? { models: [], hours: [] } : mode === "many" ? manyUsage : usage;
  return <main className="ah-room" style={{ minHeight: "100vh", padding: 20, background: "var(--vf-canvas-bg)" }}>
    <aside style={{ width: 310 }}><ProjectSummaryPanel project={project} usage={projectUsage} visibleSessionCount={128}
      activityGridStartTimestamp={start} currentTimestamp={now} ingestActive={false} censusRunning={false}
      isWorktreeVisible={true} onToggleWorktree={() => {}} /></aside>
  </main>;
}
// The page names its summary tab (?tab=models) the way the app's shell does:
// the room reads no URL, its host hands it the tab.
const initialSummaryTab = new URLSearchParams(window.location.search).get("tab") === "models" ? "models" : "activity";
createRoot(document.getElementById("root")).render(
  <RoomProvider ports={ports} options={{ initialSummaryTab }}><Fixture /></RoomProvider>,
);
