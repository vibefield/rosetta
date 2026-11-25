// The worktree list alone, at the column's width, over facts the test changes
// as a host's later answers would: a figure arriving for a row that had none.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { WorktreeList } from "../../room/src/components/worktree/WorktreeList";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/app.css";
import "../../room/src/styles/room.css";

const GB = 1024 ** 3;
const scratch = "/private/tmp/claude-501/-Users-test-Projects-project100-vibe-field/b3dfe562-aa76-4ad0-8b46-e6516b1a780a/scratchpad";
const worktrees = [
  { path: "/Users/test/Projects/field", branch: "main", head: "4082d6c9a1b2c3d4e5f60718293a4b5c6d7e8f90", gone: false, sizeBytes: 2 * GB },
  { path: "/Users/test/Projects/stack", branch: "co/items-186-187-stack-v2-with-a-name-longer-than-the-column", head: "aaaaaaa1", gone: false, sizeBytes: 6 * GB },
  { path: `${scratch}/wt-10bfix`, branch: null, head: "0e4919fff0000000000000000000000000000000", gone: false, sizeBytes: null },
  { path: `${scratch}/wt-11a-green`, branch: null, head: "8436ffab40000000000000000000000000000000", gone: false, sizeBytes: 1 * GB },
  { path: `${scratch}/wt-09-probe`, branch: null, head: "22fdbadc00000000000000000000000000000000", gone: true },
];

function Fixture() {
  const [listed, setListed] = useState(worktrees);
  const [receipt, setReceipt] = useState("");
  window.ledgerFixture = {
    measured: (path, sizeBytes) => flushSync(() => setListed((now) => now.map((w) => (w.path === path ? { ...w, sizeBytes } : w)))),
    unmeasured: () => flushSync(() => setListed(worktrees.map(({ sizeBytes: _, ...rest }) => rest))),
  };
  return <main className="ah-frame" style={{ height: "100%", padding: 24 }}>
    <div role="status">{receipt}</div>
    <div className="ah-col ah-col--rail" data-rail style={{ width: 280, height: 320, display: "flex", flexDirection: "column", background: "var(--vf-chrome-solid)" }}>
      <WorktreeList facts={{ branch: "main", dirty: 3, worktrees: listed }} openRoot="/Users/test/Projects/field"
        homeDirectory="/Users/test" onReceipt={setReceipt} />
    </div>
  </main>;
}
createRoot(document.getElementById("root")).render(<Fixture />);
