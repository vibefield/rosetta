// Real library, isolated fixture ports, and a sandboxed interactive iframe.
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow, ipcMain, nativeImage } from "electron";
import { createArtifactThumbnailRenderer } from "../electron/artifact-thumbnail-renderer.mjs";
import { createArtifactThumbnailService } from "../electron/artifact-thumbnails.mjs";
const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-artifacts-ui-"));
app.setPath("userData", path.join(temporary, "profile"));
const pulse = setInterval(() => {}, 100);
const deadline = setTimeout(() => { console.error("Artifact UI test timed out"); app.exit(1); }, 60_000);
let win, server, thumbnails, renderer;
let thumbnailRenders = 0;
let thumbnailNetworkRequests = 0;
const external = [];
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
const click = (selector) => check((s) => { document.querySelector(s).click(); }, selector);
async function until(fn, reason) {
  for (let i = 0; i < 200; i++) { if (await check(fn)) return; await pause(25); }
  throw Error(reason);
}
async function run() {
  let failed = false;
  try {
    await app.whenReady(); clearInterval(pulse);
    renderer = createArtifactThumbnailRenderer();
    thumbnails = createArtifactThumbnailService({ cacheDir: path.join(temporary, "thumbnails"), render: async (html) => { thumbnailRenders++; return renderer.render(html); } });
    ipcMain.handle("ah-artifact-thumbnail", (event, html) => event.sender === win?.webContents && event.senderFrame === win.webContents.mainFrame ? thumbnails.get(html) : null);
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error", server: { host: "127.0.0.1", port: 0, strictPort: false, open: false, hmr: false, watch: null } });
    server.middlewares.use((request, response, next) => {
      if (request.url?.startsWith("/thumbnail-probe")) { thumbnailNetworkRequests++; response.end("unexpected thumbnail request"); }
      else next();
    });
    await server.listen();
    win = new BrowserWindow({ width: 1320, height: 900, show: false, webPreferences: { preload: path.join(root, "electron/preload.cjs"), sandbox: false, contextIsolation: true, nodeIntegration: false, backgroundThrottling: false } });
    win.webContents.setWindowOpenHandler(({ url }) => { external.push(url); return { action: "deny" }; });
    await win.loadURL(`http://127.0.0.1:${server.httpServer.address().port}/test/artifacts.html`);
    await until(() => document.querySelector(".ah-artifacts-launcher"), "global artifact button missing");
    assert.equal(await check(() => window.artifactFixture.log.reads.length), 0, "opening the room must not fetch artifact bytes");
    await click(".ah-artifacts-launcher");
    await until(() => document.querySelectorAll(".ah-artifacts__select").length === 3 && document.querySelector('.ah-artifacts__list')?.getAttribute('aria-busy') === 'false', "library must load every page automatically");
    assert.equal(await check(() => [...document.querySelectorAll("button")].some((b) => b.textContent === "Load more")), false);
    await until(() => [...document.querySelectorAll(".ah-artifacts__thumbnail img")].filter((img) => img.complete && img.naturalWidth > 0).length === 2, "visible cards need static thumbnails");
    assert.equal(await check(() => document.querySelectorAll(".ah-artifacts__thumbnail iframe").length), 0, "the grid must never keep live HTML frames");
    assert.equal(BrowserWindow.getAllWindows().length, 1, "thumbnail renderer must be destroyed after its snapshot");
    assert.equal(thumbnailRenders, 1, "identical captured content must share one thumbnail render");
    assert.equal(await check(() => {
      const cards = [...document.querySelectorAll(".ah-artifacts__item")].map((e) => e.getBoundingClientRect());
      return cards[0].top === cards[1].top && cards[1].left > cards[0].right;
    }), true, "artifacts must form a preview grid");
    // The hidden macOS window needs one compositor capture before thumbnails
    // reliably paint their text into exported screenshots.
    await win.webContents.capturePage();
    for (const theme of ["dark", "light"]) {
      await check((theme) => document.documentElement.classList.toggle("dark", theme === "dark"), theme);
      await pause(200);
      writeFileSync(path.join(temporary, `grid-${theme}.png`), (await win.webContents.capturePage()).toPNG());
    }
    const readsBeforeDetail = await check(() => window.artifactFixture.log.reads.length);
    await click(".ah-artifacts__select");
    await until(() => window.artifactFixture.log.probes.length > 0, "interactive HTML did not execute");
    assert.equal(await check(() => window.artifactFixture.log.probes[0].isolated), true, "preview must not access its parent DOM");
    assert.equal(await check(() => document.body.dataset.leaked), undefined);
    assert.equal(await check(() => window.artifactFixture.log.reads.length), readsBeforeDetail + 1, "full HTML loads on opening the interactive detail");
    assert.equal(await check(() => document.querySelector("iframe").getAttribute("sandbox")), "allow-scripts");
    const frame = win.webContents.mainFrame.frames.find((frame) => frame.url === "about:srcdoc");
    assert.ok(frame, "sandboxed document frame exists");
    await frame.executeJavaScript("document.getElementById('review').click()");
    await until(() => window.artifactFixture.log.probes.some((probe) => probe.clicked), "artifact interaction did not work");
    for (const theme of ["dark", "light"]) {
      await check((theme) => document.documentElement.classList.toggle("dark", theme === "dark"), theme);
      await pause(200);
      writeFileSync(path.join(temporary, `${theme}.png`), (await win.webContents.capturePage()).toPNG());
    }
    await check(() => [...document.querySelectorAll(".ah-artifacts__toolbar button")].find((b) => b.textContent === "HTML source").click());
    await until(() => document.querySelector(".ah-artifacts__source")?.textContent.includes("A clearer path"), "source view missing");
    await check(() => [...document.querySelectorAll(".ah-artifacts__detailhead button")].find((b) => b.textContent.includes("Back to grid")).click());
    await until(() => document.activeElement?.classList.contains("ah-artifacts__select"), "returning to the grid must restore card focus");
    await click(".ah-artifacts__history");
    await until(() => document.querySelector(".ah-artifacts__filters strong") && document.querySelectorAll(".ah-artifacts__select").length === 2, "history missing");
    await check(() => document.querySelectorAll(".ah-artifacts__select")[1].click());
    await until(() => document.querySelector(".ah-artifacts__detailhead h3")?.textContent.includes("First draft"), "prior presentation missing");
    await check(() => [...document.querySelectorAll(".ah-artifacts__detailhead button")].find((b) => b.textContent.includes("Open session")).click());
    await until(() => !document.querySelector(".ah-artifacts") && window.artifactFixture.log.selections.at(-1)?.sessionId === "s1", "source session did not open");
    await until(() => document.activeElement === document.querySelector(".ah-artifacts-launcher"), "closing the dialog did not restore focus");
    await click(".ah-artifacts-launcher");
    await until(() => document.querySelectorAll(".ah-artifacts__select").length === 3, "reopen must load the entire list");
    assert.equal(thumbnailRenders, 1, "reopening the library reuses static previews");
    await check(() => document.querySelectorAll(".ah-artifacts__select")[1].click());
    await until(() => document.querySelector(".ah-artifacts__detail")?.textContent.includes("Available through its published link"), "missing HTML must offer its published page");
    assert.equal(await check(() => window.artifactFixture.log.reads.some((r) => r.id === "navigation")), false, "remote fallback must not request missing HTML");
    await click(".ah-artifacts__published");
    await pause(100);
    assert.equal(external.at(-1), "https://claude.ai/artifact/navigation-review");
    await check(() => [...document.querySelectorAll(".ah-artifacts__detailhead button")].find((b) => b.textContent.includes("Back to grid")).click());
    await until(() => document.querySelectorAll(".ah-artifacts__select").length === 3, "return to the full list failed");
    await check(() => { const input = document.querySelector('[aria-label="Search artifacts"]'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,"value").set.call(input,"no-such-design"); input.dispatchEvent(new Event("input",{bubbles:true})); });
    await until(() => document.querySelector(".ah-artifacts__list")?.textContent.includes("No matching artifacts"), "empty search state missing");
    await check(() => { const input = document.querySelector('[aria-label="Search artifacts"]'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,"value").set.call(input,""); input.dispatchEvent(new Event("input",{bubbles:true})); });
    await until(() => document.querySelectorAll(".ah-artifacts__select").length === 3, "clearing search must restore all artifacts");
    await check(() => window.artifactFixture.expire());
    const requestsBeforeRefresh = await check(() => window.artifactFixture.log.requests.length);
    await check(() => [...document.querySelectorAll(".ah-artifacts__filters button")].find((b) => b.textContent === "Refresh").click());
    await until(() => document.querySelectorAll(".ah-artifacts__select").length === 3 && document.querySelector('.ah-artifacts__list')?.getAttribute('aria-busy') === 'false', "expired cursor should recover automatically");
    assert.equal(await check(() => window.artifactFixture.log.requests.length), requestsBeforeRefresh + 4, "expired page requires a fresh complete scan");
    await check(() => window.artifactFixture.setFailure(true));
    await check(() => [...document.querySelectorAll(".ah-artifacts__filters button")].find((b) => b.textContent === "Refresh").click());
    await until(() => document.querySelector('[role="alert"]')?.textContent.includes("unavailable"), "library failure was hidden");
    await check(() => window.artifactFixture.setFailure(false));
    await check(() => [...document.querySelectorAll(".ah-artifacts__filters button")].find((b) => b.textContent === "Refresh").click());
    await until(() => document.querySelectorAll(".ah-artifacts__select").length === 3 && !document.querySelector('[role="alert"]'), "refresh did not recover the complete library");
    await check(() => window.artifactFixture.log.probes.splice(0));
    await click(".ah-artifacts__select");
    await until(() => window.artifactFixture.log.probes.length > 0, "recovered preview missing");
    win.setSize(560, 760); await pause(200);
    assert.equal(await check(() => document.querySelector(".ah-artifacts").getBoundingClientRect().right <= innerWidth), true);
    writeFileSync(path.join(temporary, "narrow.png"), (await win.webContents.capturePage()).toPNG());
    await check(() => [...document.querySelectorAll(".ah-artifacts__detailhead button")].find((b) => b.textContent.includes("Back to grid")).click());
    await pause(200);
    assert.equal(await check(() => {
      const list = document.querySelector(".ah-artifacts__list");
      return list.scrollWidth <= list.clientWidth;
    }), true, "narrow grid must not overflow horizontally");
    assert.equal(await check(() => [...document.querySelectorAll(".ah-artifacts__item")].every((card) => {
      const bounds = card.getBoundingClientRect();
      const title = card.querySelector(".ah-artifacts__itemtitle").getBoundingClientRect();
      const actions = card.querySelector(".ah-artifacts__cardactions").getBoundingClientRect();
      return title.bottom <= bounds.bottom && actions.bottom <= bounds.bottom + 1;
    })), true, "narrow cards must not clip their titles or session links");
    writeFileSync(path.join(temporary, "grid-narrow.png"), (await win.webContents.capturePage()).toPNG());
    const probe = `http://127.0.0.1:${server.httpServer.address().port}/thumbnail-probe`;
    const safe = await thumbnails.get(`<style>body{margin:0;background:rgb(0,255,0)}</style><body><img style="display:none" src="${probe}"><script>if(typeof require!=='undefined'||typeof process!=='undefined'||window.agentHistoryHost)document.body.style.background='red';fetch('${probe}').catch(()=>{});window.open('https://example.invalid/');</script></body>`);
    assert.ok(safe, "isolated thumbnail rendering failed");
    const bitmap = nativeImage.createFromDataURL(safe).toBitmap();
    assert.deepEqual([...bitmap.subarray(0, 3)], [0, 255, 0], "thumbnail rendered without Node or an app bridge");
    assert.equal(thumbnailNetworkRequests, 0, "thumbnail HTML cannot access the network or the app server");
    assert.equal(BrowserWindow.getAllWindows().length, 1, "artifact scripts must not open windows");
    const started = Date.now();
    assert.equal(await thumbnails.get("<script>for(;;){}</script>"), null, "runaway HTML must time out");
    assert.ok(Date.now() - started < 6000, "runaway render exceeded its deadline");
    assert.equal(BrowserWindow.getAllWindows().length, 1, "timed-out renderer must be destroyed");
    console.log(`Artifact UI checks passed. Screenshots: ${temporary}`);
  } catch (error) { failed = true; console.error(error); }
  finally { clearTimeout(deadline); clearInterval(pulse); thumbnails?.dispose(); renderer?.dispose(); win?.destroy(); await server?.close(); app.exit(failed ? 1 : 0); }
}
void run();
