// Production transcript and viewer with synthetic text; no daemon or archive.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { TranscriptColumn } from "../renderer/src/components/Transcript";
import { entry, inline } from "./support/ledger-fixtures.mjs";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/room.css";
import "../renderer/src/styles/transcript.css";

const nothing = () => {};
const now = 1_789_000_000_000;
const session = {
  id: "s1", native_session_id: "message-preview", runtime_id: "claude", project_id: "p1",
  cwd: "/demo", parent_session_id: null, delegation: null, root_session_id: "s1", role: "main",
  title: "Review the release handoff", summary: null, started_at: now,
  updated_at: now + 60000, stream_state: "indexed",
};
const prompt = `Please turn these notes into a clear handoff for the next release.

## What matters most

Keep the **reading experience** calm. A person should be able to understand the conversation without scrolling through pages of pasted context.

- Preserve the original wording when it carries a decision.
- Make the next action easy to find.
- Keep the supporting details available for anyone who needs them.

` + Array.from({ length: 35 }, (_, i) => `### Note ${i + 1}\n\nThe release includes changes to navigation, keyboard access, and the reading view. Review the behavior in both themes and at a narrow window width before calling it complete.`).join("\n\n") + "\n\nPROMPT END — retain every word.";
const reply = `The handoff is ready. The main decisions and next steps are below.

### Reading the conversation

Long messages should keep a readable introduction in the timeline. The full document belongs in the Markdown viewer, where there is room to read it comfortably.

1. Keep the opening paragraphs and their formatting.
2. Show a clear **View full message** action when content continues.
3. Return to the same position when the viewer closes.

` + Array.from({ length: 30 }, (_, i) => `### Review item ${i + 1}\n\nCheck prompts and replies with headings, lists, and fenced code. The preview should stay compact, and the complete message must be available in the separate reading view.`).join("\n\n") + "\n\nREPLY END — 完整消息 😀.";
const code = "Review this configuration:\n\n```ts\n" + Array.from({ length: 100 }, (_, i) => `const setting${i} = { enabled: true, label: "Option ${i}" };`).join("\n") + "\n```\n\nCODE END";
const table = "## Verification\n\n| Case | Result |\n| --- | --- |\n" + Array.from({ length: 80 }, (_, i) => `| Case ${i} | Complete |`).join("\n") + "\n\nTABLE END";
let ordinal = 0;
function message(role, text, id) {
  ordinal += 1;
  return entry({ type: role, text: inline(text), attachments: [], citations: [] }, {
    timeline_id: id, ordinal, at: now + ordinal * 1000, model: role === "agent" ? "claude-opus-4" : null,
    group: { id: `g${Math.ceil(ordinal / 2)}`, kind: role === "user" ? "human_turn" : "agent_cycle", derivation: "native" },
    response: role === "agent" ? { id: `r${ordinal}`, derivation: "native" } : null,
  });
}
const initial = [
  message("user", "Can you refine the release notes?", "short-prompt"),
  message("agent", "Yes. I’ll keep the decisions and next steps easy to find.", "short-reply"),
  message("user", prompt, "long-prompt"),
  message("agent", reply, "long-reply"),
  message("user", code, "code-prompt"),
  message("agent", table, "table-reply"),
  message("user", "Please check the result once more.", "last-prompt"),
  message("agent", "I’m checking it now.", "stream-reply"),
];

function Fixture() {
  const [rows, setRows] = useState(initial);
  const [focus, setFocus] = useState(null);
  const [currentSession, setSession] = useState(session);
  window.messageFixture = {
    prompt, reply, code, table,
    grow: () => setRows(old => old.map(row => row.timeline_id === "stream-reply"
      ? { ...row, body: { ...row.body, text: inline(reply + "\n\nSTREAM END") } } : row)),
    shorten: () => setRows(old => old.map(row => row.timeline_id === "stream-reply"
      ? { ...row, body: { ...row.body, text: inline("Complete.") } } : row)),
    seek: () => setFocus({ sessionId: "s1", timelineId: "long-reply" }),
    clearSession: () => setSession(null),
  };
  return (
    <div className="ah-frame" style={{ display: "flex", flexDirection: "column" }}>
      <div className="ah-room" style={{ position: "relative", inset: "auto", flex: 1, minHeight: 0 }}>
      <div className="ah-cols" style={{ display: "grid", gridTemplateColumns: "minmax(0, 1fr)" }}>
        <TranscriptColumn
          session={currentSession} sessions={[session]} duringBackfill={false}
          project={null} focus={focus} onFocusConsumed={() => setFocus(null)}
          feed={{ rows, snapshotReady: true, timelineMeta: null, status: "live", watermark: null,
            earlier: { kind: "none" }, earlierNotice: null, earlierAbove: 0, landed: null,
            holdEarlier: nothing, revealEarlier: nothing, retryEarlier: nothing,
            dismissEarlierNotice: nothing, canLoadEarlier: false, loadEarlier: nothing,
            loadAnchor: async () => false }}
          onReceipt={nothing} stepper={null} onStepMatch={nothing} onDismissStepper={nothing}
          getRuntimeKind={() => "claude_code"} homeDirectory={null}
        />
      </div>
      </div>
    </div>
  );
}
createRoot(document.getElementById("root")).render(<Fixture />);
