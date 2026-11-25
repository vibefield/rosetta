// The commit viewer over a change whose lines are longer than its pane: a
// Markdown table whose rows are single lines of thousands of characters, and
// one line with no break in it at all. The second file is long enough to be
// virtual, so its rows are placed by measurement rather than all drawn.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { CommitViewer } from "../../room/src/components/git/CommitViewer";
import { RoomProvider } from "../../room/src/room-context";
import { ports } from "./support/room-ports.mjs";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/app.css";
import "../../room/src/styles/room.css";

const sentence = (seed, words) =>
  Array.from({ length: words }, (_, at) => `${seed}${(at * 7 + seed.length) % 97}`).join(" ");
const row = (name, words) => `| ${name} | ${sentence(name, words)} |`;

// One hunk, as `git show` writes it: short lines around three long ones.
const table = [
  "--- a/docs/ROADMAP.md",
  "+++ b/docs/ROADMAP.md",
  "@@ -12,6 +12,7 @@ ## Now",
  " | Track | State |",
  " |---|---|",
  ` ${row("context", 420)}`,
  `-${row("before", 520)}`,
  `+${row("after", 560)}`,
  `+${"unbroken".repeat(260)}`,
  " ",
  " the last line is short",
  "",
].join("\n");

// Forty hunks of uneven lines (440 rows), so most rows are never drawn at once.
const long = ["--- a/docs/LANDED.md", "+++ b/docs/LANDED.md"];
for (let hunk = 0; hunk < 40; hunk++) {
  const start = hunk * 20 + 1;
  long.push(`@@ -${start},9 +${start},9 @@ entry ${hunk}`);
  for (let line = 0; line < 8; line++) {
    const words = (hunk * 13 + line * 29) % 5 === 0 ? 180 + ((hunk + line) % 7) * 40 : 6;
    long.push(` ${sentence(`c${hunk}x${line}`, words)}`);
  }
  long.push(`-${sentence(`old${hunk}`, 90 + (hunk % 5) * 60)}`);
  long.push(`+${sentence(`new${hunk}`, 120 + (hunk % 4) * 70)}`);
}
long.push("");

const diffs = { "docs/ROADMAP.md": table, "docs/LANDED.md": long.join("\n") };
const client = { diff: async ({ path }) => ({ error: null, text: diffs[path] }) };
const files = [
  { path: "docs/ROADMAP.md", original: null, status: "M", added: 2, deleted: 1 },
  { path: "docs/LANDED.md", original: null, status: "M", added: 40, deleted: 40 },
];
const commit = {
  sha: "4082d6c9a0000000000000000000000000000000", short: "4082d6c",
  subject: "docs: a table row is one line, however long it is",
  author: "A Reader", timestampMs: Date.UTC(2026, 10, 24, 11, 47), merge: false,
  coAuthors: [], pushed: null, added: 42, deleted: 41, files: 2,
};

function Fixture() {
  const [open, setOpen] = useState(true);
  window.diffFixture = { open: (value) => flushSync(() => setOpen(value)) };
  return <main className="ah-frame" style={{ height: "100%", padding: 24 }}>
    <p>Commit viewer · long lines</p>
    {open && <CommitViewer commit={commit} files={files} client={client} sessionPaths={new Set()}
      currentTimestamp={Date.UTC(2026, 10, 24, 12, 0)} onClose={() => setOpen(false)} onReceipt={() => {}} />}
  </main>;
}
createRoot(document.getElementById("root")).render(<RoomProvider ports={ports}><Fixture /></RoomProvider>);
