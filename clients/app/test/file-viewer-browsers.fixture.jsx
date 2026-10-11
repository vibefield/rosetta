import { useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { FileViewerDialog } from "../../room/src/components/file-viewer/FileViewerDialog";
import { RoomProvider } from "../../room/src/room-context";
import { desktopHost } from "../renderer/src/host/desktop-bridge";
import { hostPortOver } from "../renderer/src/host/rosetta-host";
import { ports } from "./support/room-ports.mjs";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/app.css";
import "../../room/src/styles/room.css";

const html = '<!doctype html>\n<html lang="en">\n  <head><title>Browser preview</title></head>\n  <body>\n    <h1>Open in your browser</h1>\n    <p>Each icon opens this document in the browser you choose.</p>\n  </body>\n</html>\n';
const bytes = new TextEncoder().encode(html);
const host = hostPortOver(desktopHost());
const receipts = [];
let releaseRead;
const source = {
  async read(handle) {
    if (handle.key === "failed") throw new Error("read unavailable");
    if (handle.key === "slow") return new Promise(resolve => { releaseRead = () => resolve(bytes); });
    return bytes;
  },
  async compare() { return { unified: "--- a/page.html\n+++ b/page.html\n@@ -1 +1 @@\n-old\n+new\n" }; },
};
function Fixture() {
  const [handle, setHandle] = useState({ key: "initial", name: "browser-preview.html", size: bytes.length, path: "/Users/test/project/browser-preview.html" });
  const [capability, setCapability] = useState("desktop");
  const [asDiff, setAsDiff] = useState(false);
  const [receipt, setReceipt] = useState("");
  window.browserFixture = {
    html, receipts,
    file: (values) => flushSync(() => setHandle(values === null ? null : { key: values.name ?? "next", name: "browser-preview.html", size: bytes.length, path: null, ...values })),
    releaseRead: () => releaseRead?.(),
    capability: value => flushSync(() => setCapability(value)),
    diff: value => flushSync(() => setAsDiff(value)),
  };
  return <main className="ah-frame" style={{ height: "100%", padding: 24 }}>
    <p>File viewer · browser controls</p>
    <div role="status">{receipt}</div>
    <FileViewerDialog handle={handle} source={source} tint="#7997d9" homeDirectory="/Users/test"
      onClose={() => setHandle(null)} asDiff={asDiff}
      listBrowsers={capability === "desktop" ? host.browsers : undefined}
      openInBrowser={capability === "none" ? null : host.openInBrowser}
      onReceipt={text => { receipts.push(text); setReceipt(text); }} />
  </main>;
}
createRoot(document.getElementById("root")).render(<RoomProvider ports={ports}><Fixture /></RoomProvider>);
