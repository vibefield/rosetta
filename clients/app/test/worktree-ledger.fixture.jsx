// The worktree list where the room draws it: the foot of the files column's
// split, 280px wide, under a tree. Sixty worktrees, one with a name far longer
// than the column, over a host the test plays: it records every ask, answers
// what it has "walked", and finishes a walk when the test says so.
import { useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { WorktreeList } from "../../room/src/components/worktree/WorktreeList";
import { RoomProvider } from "../../room/src/room-context";
import { ports } from "./support/room-ports.mjs";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/app.css";
import "../../room/src/styles/room.css";

const MB = 1024 ** 2;
const scratch = "/private/tmp/claude-501/-Users-test-Projects-project100-vibe-field/b3dfe562-aa76-4ad0-8b46-e6516b1a780a/scratchpad";
const head = (at) => `${(at * 2654435761 >>> 0).toString(16).padStart(8, "0")}00000000000000000000000000000000`;
const worktrees = [
  // the host already holds these two (kept from an earlier walk)
  { path: "/Users/test/Projects/field", branch: "main", head: head(1), gone: false, sizeBytes: 2048 * MB },
  { path: "/Users/test/Projects/stack", branch: "co/items-186-187-stack-v2-with-a-name-far-longer-than-the-column-is-wide", head: head(2), gone: false, sizeBytes: 6144 * MB },
  { path: `${scratch}/wt-10bfix`, branch: null, head: head(3), gone: false },
  { path: `${scratch}/wt-11a-green`, branch: null, head: head(4), gone: false },
  { path: `${scratch}/wt-09-probe`, branch: null, head: head(5), gone: true },
  ...Array.from({ length: 55 }, (_, at) => ({ path: `/Users/test/lanes/item-${String(at).padStart(2, "0")}`, branch: `lane/item-${String(at).padStart(2, "0")}`, head: head(at + 10), gone: false })),
];

// the host the test plays
window.asks = [];
window.walked = new Map(worktrees.filter((w) => typeof w.sizeBytes === "number").map((w) => [w.path, w.sizeBytes]));
window.hostFails = false;
const measureFor = () => async (paths) => {
  window.asks.push([...paths]);
  if (window.hostFails) throw new Error("the files service is not answering");
  return paths.map((path) => {
    if (path.endsWith("/item-03")) return { path };            // could not be read
    const bytes = window.walked.get(path);
    return bytes === undefined ? { path, sizeBytes: null } : { path, sizeBytes: bytes };
  });
};

function Fixture() {
  const [receipt, setReceipt] = useState("");
  // hosted: a host with a copy door of its own. measured: a host that walks folders.
  const [hosted, setHosted] = useState(false);
  const [measured, setMeasured] = useState(true);
  const [generation, setGeneration] = useState(1);
  const measure = useMemo(() => (measured ? measureFor(generation) : undefined), [measured, generation]);
  window.ledgerFixture = {
    hosted: (value) => flushSync(() => setHosted(value)),
    measured: (value) => flushSync(() => setMeasured(value)),
    reopen: () => flushSync(() => setGeneration((now) => now + 1)),
  };
  const list = <WorktreeList facts={{ branch: "main", dirty: 3, worktrees }} openRoot="/Users/test/Projects/field"
    homeDirectory="/Users/test" onReceipt={setReceipt} measure={measure} />;
  return <main className="ah-frame" style={{ height: "100%", padding: 24 }}>
    <div role="status">{receipt}</div>
    <div className="ah-col ah-col--rail" data-rail style={{ width: 280, height: 900, display: "flex", flexDirection: "column", overflow: "hidden", background: "var(--vf-chrome-solid)" }}>
      <div className="ah-railbody" data-split>
        {/* the tree above: one of its rows is far wider than the column, as a long file name is */}
        <div className="ah-railtree" style={{ overflow: "hidden" }}>
          <div style={{ whiteSpace: "nowrap", width: 900 }}>a-file-whose-name-is-far-wider-than-the-column.generated.snapshot.json</div>
        </div>
        {hosted ? <RoomProvider ports={hostedPorts}>{list}</RoomProvider> : list}
      </div>
    </div>
  </main>;
}
window.hostCopied = [];
const hostedPorts = { ...ports, host: {
  homeDir: "/Users/test", platform: "darwin", openInBrowser: async () => false, favicon: async () => null,
  copyText: async (text) => { if (window.hostRefuses) throw new Error("Not copied"); window.hostCopied.push(text); },
} };
createRoot(document.getElementById("root")).render(<Fixture />);
