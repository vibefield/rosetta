// Exercise the public HistoryRoom boundary, where a missing optional port
// can hide the controls even though the component's isolated tests pass.
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-ingestion-ui-"));
app.setPath("userData", path.join(temporary, "profile"));
const pulse = setInterval(() => {}, 100);
const deadline = setTimeout(() => { console.error("Import controls test timed out"); app.exit(1); }, 60_000);
let win, server;
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(fn, reason) {
  for (let i = 0; i < 200; i++) { if (await check(fn)) return; await pause(25); }
  throw Error(reason);
}
const click = () => check(() => document.querySelector(".ah-ingest-controls button").click());
async function run() {
  let failed = false;
  try {
    await app.whenReady(); clearInterval(pulse);
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error", server: { host: "127.0.0.1", port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    await server.listen();
    win = new BrowserWindow({ width: 1000, height: 850, show: false, webPreferences: { contextIsolation: true, nodeIntegration: false, backgroundThrottling: false } });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/ingestion.html`);
    await until(() => document.querySelector(".ah-cold-start .ah-ingest-controls button")?.textContent === "Resume import", "failed cold import has no Resume import button");
    assert.equal(await check(() => document.querySelector(".ah-ingest-status h3").textContent), "History import needs attention");
    assert.equal(await check(() => { const p = document.querySelector(".ah-ingest-status__detail"); return p.scrollWidth <= p.clientWidth; }), true, "long runtime identifiers must wrap inside the error panel");
    await pause(250); // Let the compositor paint the state verified above.
    writeFileSync(path.join(temporary, "failed.png"), (await win.webContents.capturePage()).toPNG());
    await click();
    await until(() => document.querySelector(".ah-ingest-controls button")?.textContent === "Pause import", "resume did not reach the host and return to running");
    const request = await check(() => window.ingestionFixture.requests[0]);
    assert.deepEqual({ ...request, command: { ...request.command, idempotencyKey: "key" } }, {
      label: "primary", action: "resume", id: "ing_resume_test", aborted: false,
      command: { indexId: "idx_resume_test", expected_revision: 5, idempotencyKey: "key" },
    });
    assert.match(request.command.idempotencyKey, /^[\da-f-]{36}$/i);
    await click();
    await until(() => document.querySelector(".ah-ingest-status h3")?.textContent === "History import paused" && document.querySelector(".ah-ingest-controls button")?.textContent === "Resume import", "pause did not return to resumable state");
    await check(() => window.ingestionFixture.authority(null));
    await until(() => !document.querySelector(".ah-ingest-controls"), "observation-only host must not offer mutations");
    await check(() => window.ingestionFixture.authority("replacement"));
    await until(() => document.querySelector(".ah-ingest-controls button")?.textContent === "Resume import", "new host authority did not reach the mounted room");
    await click();
    await until(() => window.ingestionFixture.requests.at(-1)?.label === "replacement", "resume used stale host authority");
    await check(() => window.ingestionFixture.state("running", [], "publishing"));
    await until(() => !document.querySelector(".ah-ingest-controls"), "publication fence must not offer controls");
    await check(() => window.ingestionFixture.state("failed", ["resume"]));
    await until(() => document.querySelector(".ah-ingest-controls button")?.textContent === "Resume import", "new failure hid resume");
    win.setSize(480, 850); await pause(150);
    assert.equal(await check(() => { const p = document.querySelector(".ah-ingest-status__detail"); return p.scrollWidth <= p.clientWidth; }), true);
    writeFileSync(path.join(temporary, "narrow.png"), (await win.webContents.capturePage()).toPNG());
    console.log(`Import control UI checks passed. Screenshots: ${temporary}`);
  } catch (error) { failed = true; console.error(error); }
  finally { clearTimeout(deadline); clearInterval(pulse); win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0); }
}
void run();
