// Production transcript and viewer over a mixed projection: a legacy WebP
// bundle beside recorded originals (deferred renditions, 2026-11-01). The SDK
// addresses every image unmodified; no daemon or archive.
import { RosettaClient } from "@vibefield/rosetta-protocol";
import { sameOriginAssetUrl } from "../renderer/src/host/rosetta-ports";
import { RoomProvider } from "../../room/src/room-context";
import { client, ports } from "./support/room-ports.mjs";
import { createRoot } from "react-dom/client";
import { TranscriptColumn } from "../../room/src/components/Transcript";
import { entry, inline } from "./support/ledger-fixtures.mjs";
import bundles from "./fixtures/prompt-images/optimized/manifest.json";
import { ORIGINALS } from "./image-originals-data.mjs";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/app.css";
import "../../room/src/styles/room.css";
import "../../room/src/styles/transcript.css";

// The fixture server stands in for the daemon; the room reaches it only
// through the ports, as rosetta's app reaches its own daemon.
const daemon = new RosettaClient({ baseUrl: window.location.origin });
client.baseUrl = daemon.baseUrl;
client.attachmentAssetUrl = asset => sameOriginAssetUrl(daemon, asset);
client.imageAssetUrl = daemon.imageAssetUrl.bind(daemon);
const assetReads = [];
client.readAsset = async (asset, signal) => { assetReads.push(asset.url); return daemon.readAsset(asset, signal); };

const nothing = () => {};
const now = 1_789_000_000_000;
const session = {
  id: "s1", native_session_id: "image-originals", runtime_id: "claude", project_id: "p1",
  cwd: "/demo", parent_session_id: null, delegation: null, root_session_id: "s1", role: "main",
  title: "Compare the recorded screens", summary: null, started_at: now,
  updated_at: now + 60000, stream_state: "indexed",
};
const legacy = (id, name) => ({
  id, label: `[Image #${id}]`, kind: "image", name, media_type: "image/webp",
  byte_len: bundles.landscape.asset.byte_len, dimensions: bundles.landscape.dimensions, message_ranges: [],
  source: { type: "asset", asset: bundles.landscape.asset },
});
const recorded = (id, name, key) => {
  const { digest, media_type, extension } = ORIGINALS[key];
  const asset = { url: `/api/assets/${digest}.${extension}`, digest, media_type, byte_len: 4479 };
  return { id, label: `[Image #${id}]`, kind: "image", name, media_type, byte_len: asset.byte_len,
    dimensions: bundles.portrait.dimensions, message_ranges: [], source: { type: "asset", asset } };
};
const rows = [
  entry({ type: "user", text: inline("Compare the old capture with the new ones."), attachments: [
    legacy("1", "before.png"), recorded("2", "after.png", "png"), recorded("3", "slow.jpg", "slow"),
  ] }, { timeline_id: "mixed", ordinal: 1, at: now + 1000, group: { id: "g1", kind: "human_turn", derivation: "native" } }),
];

function Fixture() {
  window.imageOriginals = { assetReads };
  return (
    <div className="ah-frame" style={{ display: "flex", flexDirection: "column" }}>
      <div className="ah-room" style={{ position: "relative", inset: "auto", flex: 1, minHeight: 0 }}>
      <div className="ah-cols" style={{ display: "grid", gridTemplateColumns: "minmax(0, 1fr)" }}>
        <TranscriptColumn
          session={session} sessions={[session]} duringBackfill={false}
          project={null} focus={null} onFocusConsumed={nothing}
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
createRoot(document.getElementById("root")).render(<RoomProvider ports={ports}><Fixture /></RoomProvider>);
