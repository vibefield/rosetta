import { useState } from "react";
import { createRoot } from "react-dom/client";
import { HistoryRoom } from "../../room/src/HistoryRoom";
import { createDemoPorts, createRoomDemo } from "../../room/src/fixtures";
import { ApiRequestError } from "../../ts/client";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/app.css";
import "../../room/src/styles/history-room.css";

const demo = createRoomDemo();
const base = createDemoPorts(demo);
const log = { requests: [], reads: [], selections: [], probes: [], thumbnails: [] };
let fail = false, expire = 0, delay = 0;
window.addEventListener("message", (event) => {
  if (event.source === document.querySelector(".ah-artifacts__preview")?.contentWindow && event.data?.kind === "artifact-probe") log.probes.push(event.data);
});
const html = `<!doctype html><html><head><style>
body{font-family:system-ui;margin:0;background:#f6f5f1;color:#242923}.page{padding:38px;max-width:900px;margin:auto}.eyebrow{font-size:11px;text-transform:uppercase;letter-spacing:.14em;color:#657966}h1{font-size:34px;letter-spacing:-1px;margin:10px 0}p{color:#6c716b;line-height:1.6}.cards{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin:28px 0}.card{background:white;border:1px solid #e0e2da;border-radius:12px;padding:20px}.card b{display:block;font-size:28px;margin:12px 0}.card span{font-size:12px;color:#72776f}button{background:#3a634b;color:white;border:0;border-radius:8px;padding:11px 18px;cursor:pointer}.foot{font-size:12px;margin-top:24px}
</style></head><body><div class="page"><div class="eyebrow">Design review · Checkout</div><h1>A clearer path to purchase.</h1><p>Three focused improvements to help customers finish with confidence.</p><div class="cards"><div class="card"><span>01 · Delivery</span><b>One view</b><span>Keep the delivery promise visible.</span></div><div class="card"><span>02 · Payment</span><b>Less work</b><span>Use a calm, focused form.</span></div><div class="card"><span>03 · Confirmation</span><b>All set</b><span>Make the next step clear.</span></div></div><button id="review">Try interaction · <span id="count">0</span></button><p class="foot">This is a captured, self-contained review page.</p></div><script>
let isolated=false;try{parent.document.body.dataset.leaked='yes'}catch{isolated=true}
parent.postMessage({kind:'artifact-probe',isolated},'*');
document.getElementById('review').onclick=()=>{document.getElementById('count').textContent=Number(document.getElementById('count').textContent)+1;parent.postMessage({kind:'artifact-probe',clicked:true},'*')};
</script></body></html>`;
function entry(id, title, sessionId, captured, group = id, presentations = 1) {
  const session = demo.meta.sessions.find((session) => session.id === sessionId);
  const runtime = demo.meta.runtimes.find((runtime) => runtime.id === session.runtime_id);
  const project = demo.meta.projects.find((project) => project.id === session.project_id);
  return {
    publication: { id, artifact_id: group, session_id: sessionId, timeline_id: `${sessionId}-t1`, title, filename: `${id}.html`, path: `/tmp/${id}.html`, url: null, native_id: null, version: null, published_at: demo.now - 3600000, source_offset: 1 },
    availability: captured ? "captured" : "missing", captured_at: demo.now, recovered_from: null,
    asset: captured ? { digest: "a".repeat(64), url: `/api/assets/${"a".repeat(64)}.html`, media_type: "text/html; charset=utf-8", byte_len: html.length } : null,
    agent: runtime.kind, session_title: session.title, project_id: project.id, project_name: project.name, presentations,
  };
}
const entries = [entry("checkout", "Checkout review", "s0", true, "checkout", 2), entry("navigation", "Navigation concepts", "s1", false), entry("dashboard", "Performance dashboard", "s2", true)];
entries[1].publication.url = "https://claude.ai/artifact/navigation-review";
entries[2].recovered_from = "/archive/dashboard.html";
const previous = entry("checkout-v1", "Checkout review · First draft", "s1", true, "checkout", 2);
const artifacts = {
  async list(request, signal) {
    log.requests.push(request);
    await new Promise((resolve) => setTimeout(resolve, delay));
    if (signal?.aborted) throw new DOMException("Aborted", "AbortError");
    if (fail) throw new Error("Fixture library is unavailable");
    if (expire && request.cursor) { expire--; throw new ApiRequestError(410, "cursor_expired", "changed"); }
    const filtered = request.artifact_id ? [entries[0], previous] : entries.filter((entry) => (!request.q || entry.publication.title.toLowerCase().includes(request.q.toLowerCase())) && (!request.agent || entry.agent === request.agent) && (!request.project || entry.project_id === request.project));
    const start = request.cursor ? 2 : 0;
    return { entries: filtered.slice(start, start + 2), next_cursor: filtered.length > start + 2 ? "page-two" : null, canonical_ready: true };
  },
  async read(sessionId, id, signal) {
    log.reads.push({ sessionId, id });
    if (signal?.aborted) throw new DOMException("Aborted", "AbortError");
    return new TextEncoder().encode(html);
  },
};
const ports = { ...base, catalog: { ...base.catalog, artifacts }, host: {
  homeDir: null, platform: "darwin", favicon: async () => null, openInBrowser: async () => false,
  artifactThumbnail: (html) => { log.thumbnails.push(html.length); return window.agentHistoryHost.artifactThumbnail(html); },
} };
window.artifactFixture = { log, entries, setFailure: (next) => { fail = next; }, expire: (count = 1) => { expire = count; }, delay: (next) => { delay = next; } };
function Fixture() {
  const [searchOpen, setSearchOpen] = useState(false);
  return <HistoryRoom {...ports} search={{ open: searchOpen, onOpenChange: setSearchOpen }} onSelect={(selection) => log.selections.push(selection)} />;
}
createRoot(document.getElementById("root")).render(<Fixture />);
