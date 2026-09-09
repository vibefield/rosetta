import { useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { createFakeEngine } from "@vibecook/mille-ui/testing";
import { WorktreeFileTree } from "../renderer/src/components/worktree/WorktreeFileTree";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/room.css";
import "@vibecook/mille-ui/tokens.css";
import "@vibecook/mille-ui/theme/vibefield.css";

const rows = [
  { id: 1, parentId: null, name: "project", kind: 1, depth: 0, hasChildren: true, isExpanded: true },
  { id: 2, parentId: 1, name: "folder", kind: 1, depth: 1, hasChildren: true, isExpanded: true },
  { id: 3, parentId: 2, name: "file ü.txt", kind: 0, depth: 2 },
  { id: 4, parentId: 1, name: "other.ts", kind: 0, depth: 1 },
].map((row) => ({
  size: 0, mtimeMs: 0, ctimeMs: 0, isIgnored: false, isReadonly: false,
  isHidden: false, hasChildren: false, isExpanded: false, ...row,
}));
const receipts = [];
const opened = [];
const engines = [];
function makeEngine(root, generation) {
  const fx = createFakeEngine();
  fx.setMirror(rows);
  engines.push(fx);
  return { fx, root, generation, dispose() {} };
}
const initialRoot = new URL(location.href).searchParams.get("worktree");

function Fixture() {
  const [engine, setEngine] = useState(() => makeEngine(initialRoot, 1));
  const [stale, setStale] = useState(false);
  const [receipt, setReceipt] = useState("");
  window.worktreeFixture = {
    receipts, opened, engines,
    switchRoot: (root, generation) => flushSync(() => setEngine(makeEngine(root, generation))),
    stale: (value) => flushSync(() => setStale(value)),
    removeFile: () => engine.fx.setMirror(rows.filter((row) => row.id !== 3)),
  };
  return (
    <main style={{ padding: 24 }}>
      <div style={{ width: 320, height: 320, display: "flex", flexDirection: "column" }}>
        <WorktreeFileTree
          engine={engine} stale={stale} openEntryId={4}
          onOpen={(entry) => opened.push(entry.id)}
          onReceipt={(message) => { receipts.push(message); setReceipt(message); }}
        />
      </div>
      <input aria-label="Text outside tree" defaultValue="normal text copy" />
      <div role="status">{receipt}</div>
    </main>
  );
}
createRoot(document.getElementById("root")).render(<Fixture />);
