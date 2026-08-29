// The Electron shell: one window holding the History room. The renderer talks
// to rosettad over localhost HTTP/WS and never touches the store; this
// process owns the daemon the way spaghetti's playground owned its cache db —
// it spawns rosettad on a db file under the per-app userData folder and hands
// the renderer the resolved origin through the `?daemon=` param the room
// already honors. The archive is the app's own; the user's first launch is
// the roll call, because a fresh db reads nothing until consent. Dev loads
// the Vite server (a loopback origin the daemon's CORS policy admits);
// packaged builds will need an app:// scheme admitted by the daemon before
// file:// is ever an origin — deliberately not wired yet.
//
// The worktree column adds the one native concern this shell owns: the mille
// engine runs in a UtilityProcess per project root (killed and re-forked on
// every switch), a MessagePort reaches the renderer through the preload, and
// read-only git facts (branch, dirty count, worktrees) answer over IPC — the
// renderer itself never touches node:fs or a shell.
//
// The file viewer adds exactly one act that leaves the app, and it is gated
// here rather than there: see `ah-file-open-external`.
import { execFile, spawn } from "node:child_process";
import { mkdirSync, realpathSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { BrowserWindow, MessageChannelMain, app, ipcMain, shell, utilityProcess } from "electron";

const here = path.dirname(fileURLToPath(import.meta.url));

// ── Single instance ──────────────────────────────────────────────────────
// Two shells would race two daemons for one db (rosettad's projection lock
// makes the loser fail, but failing is worse than focusing).
const singleInstance = app.requestSingleInstanceLock();
if (!singleInstance) {
  app.quit();
} else {
  app.on("second-instance", () => {
    const win = BrowserWindow.getAllWindows()[0];
    if (win) {
      if (win.isMinimized()) win.restore();
      win.focus();
    }
  });
}

// ── The app-owned daemon ─────────────────────────────────────────────────
// The archive db, its ledger, and the search sidecar all live beside each
// other under userData/cache — the app's own, disposable, rebuildable.
// `AH_DB` repoints the file (e.g. onto an external disk); `AH_DAEMON` skips
// the spawn entirely and uses an already-running daemon.
let daemonProcess = null;

function resolveDbPath() {
  return process.env.AH_DB ?? path.join(app.getPath("userData"), "cache", "rosetta.db");
}

/** Find a rosettad binary: explicit `ROSETTAD_BIN`, else the newest build in
 * the repo's cargo target dir (which a machine-local `target-dir` override
 * may place anywhere — ask cargo rather than guess). */
async function resolveDaemonBin() {
  if (process.env.ROSETTAD_BIN) return process.env.ROSETTAD_BIN;
  const repo = path.join(here, "..", "..", "..");
  const metadata = await new Promise((resolve) => {
    execFile(
      "cargo",
      ["metadata", "--format-version", "1", "--no-deps"],
      { cwd: repo, timeout: 10000, maxBuffer: 16 * 1024 * 1024 },
      (err, stdout) => resolve(err ? null : stdout),
    );
  });
  if (metadata === null) return null;
  let targetDir;
  try {
    targetDir = JSON.parse(metadata).target_directory;
  } catch {
    return null;
  }
  let best = null;
  for (const profile of ["debug", "release"]) {
    const candidate = path.join(targetDir, profile, "rosettad");
    try {
      const mtime = statSync(candidate).mtimeMs;
      if (best === null || mtime > best.mtime) best = { candidate, mtime };
    } catch {
      // not built in this profile
    }
  }
  return best?.candidate ?? null;
}

/** Spawn `rosettad serve` on the app's db and resolve the origin from its
 * bind-early serving line. Resolves null if it dies or stalls first — the
 * room then draws its honest disconnected state. No `--agent` flags: a fresh
 * db awaits consent, so first launch IS the roll call. */
function startDaemon(bin, dbPath) {
  mkdirSync(path.dirname(dbPath), { recursive: true });
  const proc = spawn(bin, ["serve", "--db", dbPath, "--addr", "127.0.0.1:0"], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  daemonProcess = proc;
  return new Promise((resolve) => {
    let greeting = "";
    let settled = false;
    const settle = (origin) => {
      if (!settled) {
        settled = true;
        resolve(origin);
      }
    };
    proc.stdout.on("data", (chunk) => {
      process.stdout.write(`[rosettad] ${chunk}`);
      if (settled) return;
      greeting += String(chunk);
      const served = greeting.match(/serving (http:\/\/[0-9.]+:[0-9]+)/);
      if (served) settle(served[1]);
    });
    proc.stderr.on("data", (chunk) => process.stderr.write(`[rosettad] ${chunk}`));
    proc.on("exit", (code) => {
      if (proc === daemonProcess) daemonProcess = null;
      if (!settled) console.error(`rosettad exited before serving (code ${code})`);
      settle(null);
    });
    proc.on("error", (err) => {
      if (proc === daemonProcess) daemonProcess = null;
      if (!settled) console.error(`rosettad failed to start: ${err.message}`);
      settle(null);
    });
    // The listener binds at process start; seconds of silence mean wedged.
    setTimeout(() => settle(null), 8000);
  });
}

/** SIGINT lands between files and committed progress survives for resume, so
 * a graceful stop is cheap; the hard kill is only for a wedged process. */
function stopDaemon() {
  if (daemonProcess === null) return;
  const proc = daemonProcess;
  daemonProcess = null;
  try {
    proc.kill("SIGINT");
  } catch {
    return;
  }
  const hardKill = setTimeout(() => {
    try {
      proc.kill("SIGKILL");
    } catch {
      /* already gone */
    }
  }, 3000);
  proc.on("exit", () => clearTimeout(hardKill));
}

/** The origin the renderer should talk to, threaded through the `?daemon=`
 * query param `lib/rosetta.ts` already reads. An explicit param in a dev URL
 * wins untouched. */
function withDaemonParam(url, origin) {
  if (origin === null) return url;
  const parsed = new URL(url);
  if (parsed.searchParams.has("daemon")) return url;
  parsed.searchParams.set("daemon", origin);
  return parsed.toString();
}

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

function createWindow(daemonOrigin) {
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
  if (devUrl) win.loadURL(withDaemonParam(devUrl, daemonOrigin));
  else if (daemonOrigin !== null)
    win.loadFile(path.join(here, "../dist/index.html"), { query: { daemon: daemonOrigin } });
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

app.whenReady().then(async () => {
  if (!singleInstance) return;

  // An explicit daemon — `AH_DAEMON`, or a `?daemon=` already in the dev
  // URL (the smoke recipes) — means someone else owns the archive; only
  // otherwise does the app raise its own.
  const devUrl = process.env.ELECTRON_RENDERER_URL;
  let daemonOrigin = process.env.AH_DAEMON ?? null;
  const devHasDaemon = devUrl ? new URL(devUrl).searchParams.has("daemon") : false;
  if (daemonOrigin === null && !devHasDaemon) {
    const bin = await resolveDaemonBin();
    if (bin === null) {
      console.error("no rosettad binary — set ROSETTAD_BIN or `cargo build -p rosettad`");
    } else {
      daemonOrigin = await startDaemon(bin, resolveDbPath());
    }
  }

  const win = createWindow(daemonOrigin);

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

  // The file viewer's one outward act: hand an HTML or SVG file to the
  // browser. Four gates, in this order, because the renderer is showing
  // content from a repository nobody here wrote and is therefore the process
  // whose word on "open this" is worth least:
  //
  //   1. the extension is one the OS opens in a browser and never executes;
  //   2. the real path — symlinks resolved — is inside the open worktree,
  //      so a link in a repo cannot walk out of it;
  //   3. it is a regular file;
  //   4. `shell.openPath`, which respects the default handler and, for these
  //      extensions, cannot launch anything.
  //
  // Anything else answers false, which the room reports to the reader. A
  // silent no-op would read as a broken button — which is also why the
  // OS's own answer is awaited rather than discarded: `openPath` resolves to
  // a reason when nothing opened, and reporting success over it would be the
  // one place this gate lied.
  ipcMain.handle("ah-file-open-external", async (_evt, raw) => {
    if (typeof raw !== "string" || raw === "" || worktreeRoot === null) return false;
    if (!/\.(html?|svg)$/i.test(raw)) return false;
    try {
      const target = realpathSync(raw);
      const root = realpathSync(worktreeRoot);
      const inside = target === root || target.startsWith(root + path.sep);
      if (!inside || !statSync(target).isFile()) return false;
      return (await shell.openPath(target)) === "";
    } catch {
      return false;
    }
  });

  const smokeAt = process.argv.indexOf("--smoke");
  if (smokeAt !== -1) {
    const out = process.argv[smokeAt + 1] ?? "smoke.png";
    const delayMs = Number(process.argv[smokeAt + 2] ?? 2500);
    smoke(win, out, delayMs);
  }
  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow(daemonOrigin);
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
  stopDaemon();
});
