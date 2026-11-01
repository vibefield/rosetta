// A short-lived offscreen renderer produces one inert bitmap, then is destroyed.
// It has no preload, app cookies, Node bridge, permissions, or network access.
import { randomUUID } from "node:crypto";
import { BrowserWindow, session } from "electron";

export function createArtifactThumbnailRenderer({ timeoutMs = 4000, settleMs = 350 } = {}) {
  const isolated = session.fromPartition(`artifact-thumbnails-${randomUUID()}`, { cache: false });
  isolated.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
  isolated.setPermissionCheckHandler(() => false);
  isolated.webRequest.onBeforeRequest((details, callback) => callback({ cancel: !details.url.startsWith("data:") && !details.url.startsWith("blob:") }));
  isolated.on("will-download", (event) => event.preventDefault());
  let active = null, stopped = false;

  async function render(html) {
    if (stopped) return null;
    const win = new BrowserWindow({
      width: 1200, height: 750, show: false, backgroundColor: "#ffffff",
      webPreferences: { session: isolated, offscreen: true, sandbox: true, contextIsolation: true, nodeIntegration: false, webSecurity: true, backgroundThrottling: false, spellcheck: false },
    });
    active = win;
    const contents = win.webContents;
    contents.setFrameRate(10);
    contents.setAudioMuted(true);
    contents.setWindowOpenHandler(() => ({ action: "deny" }));
    contents.on("will-navigate", (event) => event.preventDefault());
    contents.on("will-frame-navigate", (event) => event.preventDefault());
    contents.on("will-attach-webview", (event) => event.preventDefault());
    const policy = "default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src 'none'; connect-src 'none'; frame-src 'none'; worker-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
    const document = `<!doctype html><meta http-equiv="Content-Security-Policy" content="${policy}"><meta name="referrer" content="no-referrer">${html}`;
    let timer;
    try {
      return await Promise.race([
        (async () => {
          await contents.loadURL(`data:text/html;charset=utf-8;base64,${Buffer.from(document).toString("base64")}`);
          await contents.insertCSS("* { animation-play-state: paused !important; transition: none !important; caret-color: transparent !important; }");
          await new Promise((resolve) => setTimeout(resolve, settleMs));
          const image = await contents.capturePage(undefined, { stayHidden: true, stayAwake: true });
          return image.isEmpty() ? null : image.resize({ width: 600, height: 375 }).toPNG();
        })(),
        new Promise((resolve) => { timer = setTimeout(() => resolve(null), timeoutMs); }),
      ]);
    } catch { return null; }
    finally {
      clearTimeout(timer);
      if (!win.isDestroyed()) win.destroy();
      if (active === win) active = null;
    }
  }
  return { render, dispose() { stopped = true; if (active && !active.isDestroyed()) active.destroy(); active = null; } };
}
