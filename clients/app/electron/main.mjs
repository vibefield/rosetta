// The Electron shell: one window holding the History room. The renderer talks
// to rosettad over localhost HTTP/WS; nothing in this process touches the
// daemon or the store. Dev loads the Vite server (a loopback origin the
// daemon's CORS policy admits); packaged builds will need an app:// scheme
// admitted by the daemon before file:// is ever an origin — deliberately not
// wired yet.
import path from "node:path";
import { fileURLToPath } from "node:url";
import { BrowserWindow, app, shell } from "electron";

const here = path.dirname(fileURLToPath(import.meta.url));

function createWindow() {
  const win = new BrowserWindow({
    width: 1500,
    height: 940,
    minWidth: 980,
    minHeight: 620,
    title: "Agent History",
    titleBarStyle: process.platform === "darwin" ? "hiddenInset" : "default",
    backgroundColor: "#fafafa",
    webPreferences: {
      preload: path.join(here, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });

  // External links leave the app; the room never navigates away.
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith("https://") || url.startsWith("http://")) shell.openExternal(url);
    return { action: "deny" };
  });

  const devUrl = process.env.ELECTRON_RENDERER_URL;
  if (devUrl) win.loadURL(devUrl);
  else win.loadFile(path.join(here, "../dist/index.html"));
  return win;
}

/** `--smoke out.png [delayMs]` — load, settle, capture, quit. A build gate,
 * not a feature: it is how a headless session proves the room draws. */
async function smoke(win, out, delayMs) {
  await new Promise((resolve) => win.webContents.once("did-finish-load", resolve));
  await new Promise((resolve) => setTimeout(resolve, delayMs));
  const image = await win.webContents.capturePage();
  const { writeFileSync } = await import("node:fs");
  writeFileSync(out, image.toPNG());
  console.log(`smoke: wrote ${out}`);
  app.quit();
}

app.whenReady().then(() => {
  const win = createWindow();
  const smokeAt = process.argv.indexOf("--smoke");
  if (smokeAt !== -1) {
    const out = process.argv[smokeAt + 1] ?? "smoke.png";
    const delayMs = Number(process.argv[smokeAt + 2] ?? 2500);
    smoke(win, out, delayMs);
  }
  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
