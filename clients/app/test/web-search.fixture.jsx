// Production ledger components, deterministic source records, no external requests.
import { RoomProvider } from "../renderer/src/room-context";
import { ports } from "./support/room-ports.mjs";
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { Line } from "../renderer/src/components/transcript/Ledger";
import { Favicon } from "../renderer/src/components/transcript/Favicon";
import { ledgerLines } from "../renderer/src/lib/transcript/ledger";
import { line, url } from "./support/ledger-fixtures.mjs";
import { setSiteIconsEnabled } from "../renderer/src/lib/transcript/favicons";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/room.css";
import "../renderer/src/styles/transcript.css";

const calls = [];
const pending = [];
const icon = (color) => `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16" rx="3" fill="${color}"/><circle cx="8" cy="8" r="4" fill="white"/></svg>`)}`;
window.agentHistoryHost = {
  favicon: async (host) => {
    calls.push(host);
    if (host === "pending.example.org") return new Promise(resolve => pending.push(resolve));
    if (host === "broken.example.org") return "data:image/png;base64,AA==";
    if (host === "toji.dev") return null;
    return icon(host === "github.com" ? "#4d79c7" : "#e08d40");
  },
};
const nothing = () => { };
const sources = [
  { url: "https://github.com/gpuweb/gpuweb", title: "gpuweb/gpuweb: Where the GPU for the Web work happens" },
  { url: "https://developer.mozilla.org/en-US/docs/Web/API/WebGPU_API", title: "WebGPU API — Web APIs | MDN" },
  { url: "https://stackoverflow.com/questions/123", title: "How to do vertex pulling in WGSL without a vertex buffer?" },
  { url: "https://www.w3.org/TR/WGSL/", title: "WebGPU Shading Language — W3C Working Draft" },
  { url: "https://toji.dev/webgpu", title: "Rendering 100k dots with instancing and a single draw" },
];
const search = (rows, total = rows?.length) => line({ type: "web_search", query: "webgpu vertex pulling instanced needles", allowed_domains: [], blocked_domains: [], outcome: rows === null ? null : { sources: rows, total_count: total } });
const fetch = (target, title, status = 200, over = {}) => line({ type: "web_fetch", url: url(target), purpose: null, outcome: { final_url: target, title, http_status: status } }, over);
const long = Array.from({ length: 30 }, (_, i) => ({ url: `https://github.com/page-${i}`, title: `Recorded result ${i + 1}` }));
const cases = [
  ["search", search(sources)],
  ["fetch", fetch(sources[1].url, sources[1].title)],
  ["failed", fetch("https://toji.dev/missing", null, 404, { status: "failed", failure: { kind: "unknown", summary: null, diagnostic: "The server returned Not Found." } })],
  ["fold", ledgerLines([search(sources.slice(0, 2), 5), search(sources.slice(2), 7)].flatMap(line => line.rows))[0]],
  ["many", search(long, 36)],
  ["empty", search([])],
  ["missing", search(null)],
  ["invalid", search([{ url: "javascript:window.badLink=true", title: "Unusable recorded address" }])],
  ["fetches", ledgerLines([fetch(sources[0].url, sources[0].title), fetch(sources[1].url, sources[1].title)].flatMap(line => line.rows))[0]],
  ["long-domain", search([{ url: `https://${"long-".repeat(10)}.example.org/page`, title: "A title beside a very long domain name" }])],
];

function Fixture() {
  const [host, setHost] = useState(null);
  window.webFixture = { calls, pending, icon, enable: setSiteIconsEnabled, probe(value) { flushSync(() => setHost(value)); } };
  return <main className="ah-frame" style={{ overflow: "auto" }}>
    <div className="tr-col"><div className="tr">
      <h3 style={{ fontSize: 14, fontWeight: 500 }}>Web search · web fetch</h3>
      <div className="tr-ledger">
        {cases.map(([id, model]) => <div data-case={id} key={id}>
          <Line line={model} domId={`web-${id}`} hit={false} alive={false} forceOpen={false} subthreads={[]} workflowAttempts={[]} hasChildren={false} onOpenLinkedChild={null} onOpenPath={nothing} />
        </div>)}
      </div>
      {host && <div data-probe><Favicon host={host} /></div>}
    </div></div>
  </main>;
}
createRoot(document.getElementById("root")).render(<RoomProvider ports={ports}><Fixture /></RoomProvider>);
