// The Electron shell: one window holding the History room. The renderer talks
// to rosettad over localhost HTTP/WS; nothing in this process touches the
// daemon or the store. Dev loads the Vite server (a loopback origin the
// daemon's CORS policy admits); packaged builds will need an app:// scheme
// admitted by the daemon before file:// is ever an origin — deliberately not
// wired yet.
//
// The worktree column adds the one native concern this shell owns: the mille
// engine runs in a UtilityProcess per project root (killed and re-forked on
// every switch), a MessagePort reaches the renderer through the preload, and
// read-only git facts (branch, dirty count, worktrees) answer over IPC — the
// renderer itself never touches node:fs or a shell.
import { execFile } from "node:child_process";
import { statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { BrowserWindow, MessageChannelMain, app, ipcMain, shell, utilityProcess } from "electron";

const here = path.dirname(fileURLToPath(import.meta.url));

/** The one root the worktree column is currently allowed to answer git
 * questions for. IPC never trusts a renderer-supplied path beyond equality
 * with this. */
let worktreeRoot = null;
let fxProcess = null;

function forkFxHost(root) {
  const proc = utilityProcess.fork(path.join(here, "fx-host.mjs"), [], {
    serviceName: "agent-history-worktree",
    stdio: "pipe",
    env: { ...process.env, AH_WORKTREE_ROOT: root },
  });
  proc.stdout?.on("data", (d) => process.stdout.write(`[fx-host] ${d}`));
  proc.stderr?.on("data", (d) => process.stderr.write(`[fx-host] ${d}`));
  return proc;
}

/** Start (or restart) the engine against `root` and hand the window a fresh
 * port. The old utility dies first so its watchers release; the port only
 * transfers after the utility's `ready`, or Node's parent-channel delivery
 * can drop it. */
function openWorktree(win, root) {
  if (fxProcess !== null) {
    try {
      fxProcess.kill();
    } catch {
      /* already dead */
    }
    fxProcess = null;
  }
  worktreeRoot = root;
  const proc = forkFxHost(root);
  fxProcess = proc;
  const onMessage = (msg) => {
    if (msg?.type !== "ready") return;
    proc.off("message", onMessage);
    if (proc !== fxProcess) return; // superseded by a newer switch
    const { port1, port2 } = new MessageChannelMain();
    proc.postMessage({ type: "attach" }, [port1]);
    win.webContents.postMessage("ah-worktree-port", { root }, [port2]);
  };
  proc.on("message", onMessage);
}

function git(root, args) {
  return new Promise((resolve) => {
    execFile("git", ["-C", root, ...args], { timeout: 4000 }, (err, stdout) => {
      resolve(err ? null : stdout);
    });
  });
}

/** Read-only git facts for the open worktree root: branch, dirty count, and the
 * repository's worktrees. Absence is honest — a folder without git answers
 * with nulls, never an error. */
async function gitFacts(root) {
  const branch = (await git(root, ["rev-parse", "--abbrev-ref", "HEAD"]))?.trim() ?? null;
  const status = await git(root, ["status", "--porcelain"]);
  const dirty = status === null ? null : status.split("\n").filter((l) => l !== "").length;
  const worktrees = [];
  const listed = await git(root, ["worktree", "list", "--porcelain"]);
  if (listed !== null) {
    for (const block of listed.split("\n\n")) {
      const lines = block.split("\n");
      const p = lines.find((l) => l.startsWith("worktree "))?.slice(9);
      const b = lines.find((l) => l.startsWith("branch "))?.slice(7).replace("refs/heads/", "");
      if (p) worktrees.push({ path: p, branch: b ?? null });
    }
  }
  return { branch, dirty, worktrees };
}

function createWindow() {
  // `--frame WxH` (smoke only): prove H-3 at a chosen width.
  const frameAt = process.argv.indexOf("--frame");
  const frame = frameAt !== -1 ? process.argv[frameAt + 1]?.split("x").map(Number) : null;
  const win = new BrowserWindow({
    width: frame?.[0] || 1500,
    height: frame?.[1] || 940,
    minWidth: 980,
    minHeight: 620,
    title: "Agent History",
    titleBarStyle: process.platform === "darwin" ? "hiddenInset" : "default",
    backgroundColor: "#fafafa",
    webPreferences: {
      preload: path.join(here, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      // mille's port handshake runs through the preload; the embedding guide
      // requires an unsandboxed preload while context isolation stays on.
      sandbox: false,
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
  // An occluded window stops painting, and `capturePage` then returns the
  // blank frame rather than the room. Raise it first — a smoke shot that
  // silently captures nothing is worse than no gate at all.
  win.setAlwaysOnTop(true);
  win.show();
  win.focus();
  await new Promise((resolve) => setTimeout(resolve, 900));
  const image = await win.webContents.capturePage();
  const { writeFileSync } = await import("node:fs");
  writeFileSync(out, image.toPNG());
  console.log(`smoke: wrote ${out}`);
  app.quit();
}

app.whenReady().then(() => {
  const win = createWindow();

  // The renderer names the project root it is reading (learned from
  // rosettad); this process validates it is a real directory before any
  // walker or git call sees it.
  ipcMain.handle("ah-worktree-open", (_evt, raw) => {
    if (typeof raw !== "string" || raw.length === 0) return false;
    try {
      if (!statSync(raw).isDirectory()) return false;
    } catch {
      return false;
    }
    openWorktree(win, raw);
    return true;
  });

  ipcMain.handle("ah-worktree-git-facts", async (_evt, raw) => {
    if (typeof raw !== "string" || raw !== worktreeRoot) return null;
    return gitFacts(raw);
  });

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

app.on("before-quit", () => {
  if (fxProcess !== null) {
    try {
      fxProcess.kill();
    } catch {
      /* already dead */
    }
    fxProcess = null;
  }
});
