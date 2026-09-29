// Production transcript and viewer with synthetic text; no daemon or archive.
import { client } from "../renderer/src/lib/rosetta";
client.setBaseUrl(window.location.origin);
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { TranscriptColumn } from "../renderer/src/components/Transcript";
import { entry, inline, tool, file } from "./support/ledger-fixtures.mjs";
import { memberText } from "./prompt-images-data.mjs";
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
const retryCase = Date.now(); // Disposable fixture request identity, not normalization.
const imageRoot = "/test/fixtures/prompt-images/";
const image = (id, name, ranges = [], source = null) => ({
  id, label: `[Image #${id}]`, kind: "image", name, media_type: "image/png", byte_len: name === "portrait.png" ? 3167 : 4479,
  dimensions: name === "portrait.png" ? { width: 320, height: 640 } : { width: 960, height: 480 },
  message_ranges: ranges,
  source: source ?? { type: "asset", asset: { url: imageRoot + name + `?case=${retryCase}`, digest: id.padStart(64, "0"), media_type: "image/png", byte_len: name === "portrait.png" ? 3167 : 4479 } },
});
const markerRanges = (text, marker) => {
  const ranges = [];
  for (let at = text.indexOf(marker); at !== -1; at = text.indexOf(marker, at + marker.length))
    ranges.push({ start: [...text.slice(0, at)].length, end: [...text.slice(0, at + marker.length)].length });
  return ranges;
};
let ordinal = 0;
function message(text, id, attachments) {
  ordinal += 1;
  return entry({ type: "user", text: text === null ? null : inline(text), attachments }, {
    timeline_id: id, ordinal, at: now + ordinal * 1000,
    group: { id: `g${ordinal}`, kind: "human_turn", derivation: "native" },
  });
}
function activity() {
  const peerText = "Compare [Image #1]; [Image #2] was not recorded.\n\n" + "Review the image and retain these implementation notes.\n\n".repeat(80);
  const delivery = (id, channel, kind, text, attachments) => entry({ type: "collaboration", event: {
    kind, channel, sender: channel === "coordinator" ? "coordinator" : "reviewer", recipient: null,
    team: null, color: null, summary: null, body: text === null ? null : inline(text), attachments,
    task_id: kind === "report" ? "review-task" : null, delivery_id: null, status: null, authored_at: null,
    source_session_id: kind === "report" ? "s1" : null, source_response_id: kind === "report" ? "review-response" : null,
    work_start_timeline_id: null, work_end_timeline_id: null,
  } }, { timeline_id: id });
  const rows = [
    tool({ type: "file_read", targets: [
      { resource: file("/demo/README.md"), range: null, media_kind: null },
      ...Array.from({ length: 5 }, (_, index) => ({ resource: file(`/demo/capture-${index}.png`), range: null, media_kind: "image" })),
    ], outcome: null }, { timeline_id: "image-read" }),
    tool({ type: "artifact", operation: "create", artifact_kind: "image", name: "Revised view", description: null,
      resources: [file("/demo/revised.png")], media: { media_type: "image/png", width: 960, height: 480, duration_ms: null } }, { timeline_id: "image-artifact" }),
    delivery("image-team", "team", "message", "[Image #1] shows the team’s review.", [image("1", "portrait.png", [{ start: 0, end: 10 }])]),
    delivery("image-peer", "peer", "message", peerText, [
      image("1", "landscape.png", markerRanges(peerText, "[Image #1]")),
      image("2", "absent.png", markerRanges(peerText, "[Image #2]"), { type: "unavailable", reason: "not_recorded" }),
    ]),
    delivery("image-report", "team", "report", "[Image #1] is the finished design.", [image("1", "portrait.png", [{ start: 0, end: 10 }])]),
    entry({ type: "user", text: inline("Inspect [Image #1]."), attachments: [image("1", "landscape.png", [{ start: 8, end: 18 }])] }, { timeline_id: "image-assignment" }),
    delivery("image-coordinator", "coordinator", "message", "Use [Image #1].", [image("1", "portrait.png", [{ start: 4, end: 14 }])]),
    delivery("image-peer-only", "peer", "message", null, [image("1", "landscape.png")]),
    entry({ type: "agent", text: inline("The revised screens keep the reading area quiet. Here is the final comparison."), attachments: [image("reply-image", "landscape.png")], citations: [] }, { timeline_id: "image-reply" }),
  ];
  return rows.map(row => ({ ...row, ordinal: ++ordinal, at: now + ordinal * 1000,
    group: { id: "image-activity", kind: row.timeline_id === "image-assignment" ? "parent_invocation" : "agent_cycle", derivation: "native" } }));
}
const single = "😀 [Image #1] Can we make these screens easier to scan?";
const long = "Please review [Image #1].\n\n" + "## Review notes\n\nKeep the image beside the original prompt and preserve every detail.\n\n".repeat(36)
  + "Full message end. Compare [Image #1] again.\n\n`[Image #1]` is code, and [Image #1](https://example.com) is an ordinary link.";
const initial = [
  message(single, "single", [image("1", "landscape.png", markerRanges(single, "[Image #1]"))]),
  message("Six views of the same screen.", "gallery", Array.from({ length: 6 }, (_, i) => image(String(i + 1), i % 2 ? "portrait.png" : "landscape.png"))),
  ...activity(),
  message(null, "image-only", [image("1", "portrait.png")]),
  message("[Image #1] is missing; [Image #4] was recorded.", "missing", [
    image("1", "absent.png", [{ start: 0, end: 10 }], { type: "unavailable", reason: "not_recorded" }),
    image("4", "landscape.png", [{ start: 23, end: 33 }]),
  ]),
  message("This image needs one retry to load.", "failed", [image("1", "retry.png")]),
  message(long, "long", [image("1", "portrait.png", markerRanges(long, "[Image #1]"))]),
];
const member = message(memberText, "member", [image("1", "portrait.png", markerRanges(memberText, "[Image #1]"))]);
member.body.text = { type: "member", preview: memberText.slice(0, 220), byte_len: new TextEncoder().encode(memberText).length,
  asset: { url: `${imageRoot}full-message.md?case=${retryCase}`, digest: "f".repeat(64), media_type: "text/plain; charset=utf-8", byte_len: new TextEncoder().encode(memberText).length } };
initial.push(member);

function Fixture() {
  const [rows, setRows] = useState(initial);
  const [focus, setFocus] = useState(null);
  const [currentSession, setSession] = useState(session);
  window.imageFixture = {
    long, rows, clearSession: () => setSession(null),
    repair: () => setRows(old => old.map(row => row.timeline_id === "failed"
      ? { ...row, body: { ...row.body, attachments: [image("1", "landscape.png")] } } : row)),
  };
  return (
    <div className="ah-frame" style={{ display: "flex", flexDirection: "column" }}>
      <div style={{ position: "fixed", top: 50, right: 12, zIndex: 5 }}>
        <button onClick={() => document.documentElement.classList.toggle("dark")}>Toggle test theme</button>
      </div>
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
          onReceipt={text => { window.lastReceipt = text; }} stepper={null} onStepMatch={nothing} onDismissStepper={nothing}
          getRuntimeKind={() => "claude_code"} homeDirectory={null}
        />
      </div>
      </div>
    </div>
  );
}
createRoot(document.getElementById("root")).render(<Fixture />);
