// Real React/Mille menu and keyboard events, production preload/IPC handler,
// and the system clipboard. Finder calls are recorded to avoid opening test
// folders on the user's desktop. No daemon or archive is involved.
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow, clipboard, ipcMain } from "electron";
import { createWorktreeFileActionHandler } from "../electron/worktree-file-actions.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const temporary = mkdtempSync(path.join(os.tmpdir(), "rosetta-worktree-ui-"));
const firstRoot = path.join(temporary, "first project");
const secondRoot = path.join(temporary, "second project");
for (const directory of [firstRoot, secondRoot]) {
  mkdirSync(path.join(directory, "folder"), { recursive: true });
  writeFileSync(path.join(directory, "folder", "file ü.txt"), "fixture");
  writeFileSync(path.join(directory, "other.ts"), "fixture");
}
app.setPath("userData", path.join(temporary, "profile"));
const pulse = setInterval(() => {}, 50);
const deadline = setTimeout(() => { console.error("Worktree UI test timed out"); app.exit(1); }, 90_000);
let server;
let win;
let session;
let clipboardBackup;
let lastClipboardWrite;
let failed = false;
const errors = [];
const revealed = [];
const screenshots = [];
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const check = (fn, ...args) => win.webContents.executeJavaScript(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function until(predicate, message) {
  for (let i = 0; i < 160; i++) {
    if (await predicate()) return;
    await pause(25);
  }
  throw new Error(message);
}
async function key(keyCode, modifiers = []) {
  win.webContents.sendInputEvent({ type: "keyDown", keyCode, modifiers });
  win.webContents.sendInputEvent({ type: "keyUp", keyCode, modifiers });
  await pause(30);
}
async function click(selector, button = "left", modifiers = []) {
  const point = await check((s) => {
    const el = document.querySelector(s);
    if (!el) throw new Error(`Missing element ${s}`);
    const rect = el.getBoundingClientRect();
    return { x: Math.round(rect.x + Math.min(60, rect.width / 2)), y: Math.round(rect.y + rect.height / 2) };
  }, selector);
  win.webContents.sendInputEvent({ type: "mouseDown", ...point, button, clickCount: 1, modifiers });
  win.webContents.sendInputEvent({ type: "mouseUp", ...point, button, clickCount: 1, modifiers });
  await pause(40);
}
const row = (id) => `[data-mille-row-id="${id}"]`;
const menuItem = (id) => `[role="menuitem"][data-mille-command-id="${id}"]`;
async function copied(expected) {
  await until(() => clipboard.readText() === expected, `Clipboard did not receive ${expected}`);
  lastClipboardWrite = expected;
}
async function menu() {
  await until(() => check(() => document.querySelectorAll('[role="menuitem"]').length === 2), "Expected exactly two menu actions");
  assert.deepEqual(await check(() => [...document.querySelectorAll(".mille-context-menu-item-label")].map((el) => el.textContent)),
    ["Copy Path", process.platform === "darwin" ? "Open in Finder" : "Open in File Manager"]);
}

async function run() {
  try {
    await app.whenReady();
    clearInterval(pulse);
    // Preserve Electron's supported clipboard formats, without printing or
    // persisting their contents. Don't restore if the user copies new text.
    clipboardBackup = { text: clipboard.readText(), html: clipboard.readHTML(), rtf: clipboard.readRTF(), image: clipboard.readImage() };
    const { createServer } = await import("vite");
    server = await createServer({ configFile: path.join(root, "vite.config.ts"), root, logLevel: "error",
      server: { host: "127.0.0.1", port: 0, strictPort: false, open: false } });
    await server.listen();
    win = new BrowserWindow({ width: 720, height: 600, show: false,
      webPreferences: { preload: path.join(root, "electron/preload.cjs"), contextIsolation: true,
        nodeIntegration: false, sandbox: false, backgroundThrottling: false } });
    session = { root: firstRoot, generation: 1, contents: win.webContents, ready: true, stopping: false };
    ipcMain.handle("ah-worktree-file-action", createWorktreeFileActionHandler({
      getSession: () => session,
      writeClipboard: (text) => { lastClipboardWrite = text; clipboard.writeText(text); },
      revealInFileManager: (target) => revealed.push(target),
    }));
    win.webContents.on("console-message", (event) => { if (event.level === "error") errors.push(event.message); });
    win.webContents.on("render-process-gone", (_event, details) => errors.push(JSON.stringify(details)));
    const port = server.httpServer.address().port;
    await win.loadURL(`http://127.0.0.1:${port}/test/worktree-actions.html?worktree=${encodeURIComponent(firstRoot)}`);
    await until(() => check(() => !!document.querySelector('[data-mille-row-id="1"]')), "Tree did not render");
    for (const id of [1, 2]) {
      if (await check((s) => document.querySelector(s)?.getAttribute("aria-expanded") !== "true", row(id))) await click(row(id));
    }
    await until(() => check(() => !!document.querySelector('[data-mille-row-id="3"]')), "Nested file did not render");

    await click(row(3));
    await key("c", ["control"]);
    await copied(path.join(firstRoot, "folder/file ü.txt"));
    assert.deepEqual(await check(() => window.worktreeFixture.opened), [], "Copy must use selection without opening the viewer");
    await click(row(4));
    await key("c", ["meta"]);
    await copied(path.join(firstRoot, "other.ts"));
    // macOS's native Edit > Copy dispatches a copy event rather than a
    // synthetic keydown. Exercise that route with the same selected file.
    await click(row(3));
    win.webContents.copy();
    await copied(path.join(firstRoot, "folder/file ü.txt"));
    await click(row(4));
    await click(row(3), "left", ["meta"]);
    await key("c", ["control"]);
    const multiple = ["folder/file ü.txt", "other.ts"].map((p) => path.join(firstRoot, p)).join("\n");
    await copied(multiple);
    await click(row(3), "right");
    await menu();
    assert.deepEqual(await check(() => [...document.querySelectorAll('[data-mille-selected="true"]')].map((el) => Number(el.dataset.milleRowId))), [3, 4], "Right-click within selection must preserve it");
    await click(menuItem("file.revealInFileManager"));
    await until(() => revealed.length === 1, "Selected item's Finder action did not reach main");
    assert.deepEqual(revealed, [path.join(firstRoot, "folder/file ü.txt")]);

    await click(row(4));
    await click(row(3), "right");
    await menu();
    assert.deepEqual(await check(() => [...document.querySelectorAll('[data-mille-selected="true"]')].map((el) => Number(el.dataset.milleRowId))), [3]);
    for (const theme of ["light", "dark"]) {
      await check((value) => { document.documentElement.dataset.theme = value; document.documentElement.classList.toggle("dark", value === "dark"); }, theme);
      await pause(80); // capture a painted frame after the token change
      const screenshot = path.join(temporary, `menu-${theme}.png`);
      await fs.writeFile(screenshot, (await win.webContents.capturePage()).toPNG());
      screenshots.push(screenshot);
    }
    await click(menuItem("file.copyAbsolutePath"));
    await copied(path.join(firstRoot, "folder/file ü.txt"));
    await until(() => check(() => !document.querySelector('[role="menu"]')), "Menu did not close");

    await click(row(4));
    await key("F10", ["shift"]);
    await menu();
    await click(menuItem("file.revealInFileManager"));
    await until(() => revealed.length === 2, "Finder request did not reach main");
    assert.deepEqual(revealed, [path.join(firstRoot, "folder/file ü.txt"), path.join(firstRoot, "other.ts")]);

    // Empty selection and a stale tree must leave the clipboard alone.
    await click(row(3));
    await key("Escape");
    await key("c", ["control"]);
    assert.equal(clipboard.readText(), path.join(firstRoot, "folder/file ü.txt"));
    await click(row(4));
    await check(() => window.worktreeFixture.stale(true));
    await key("c", ["control"]);
    assert.equal(clipboard.readText(), path.join(firstRoot, "folder/file ü.txt"));
    await check(() => window.worktreeFixture.stale(false));

    // sendInputEvent doesn't execute macOS native editing commands (Electron
    // #6338). Check that the key remains unhandled, then invoke native Copy.
    await check(() => {
      const input = document.querySelector("input");
      input.focus(); input.select();
      window.outsideCopyPrevented = null;
      document.addEventListener("keydown", (event) => {
        window.outsideCopyPrevented = event.defaultPrevented;
      }, { once: true });
    });
    await key("c", [process.platform === "darwin" ? "meta" : "control"]);
    assert.equal(await check(() => window.outsideCopyPrevented), false);
    win.webContents.copy();
    await copied("normal text copy");

    await click(row(3));
    for (const [code, modifiers] of [["F2", []], ["Delete", []], ["Backspace", ["shift"]],
      ...["control", "meta"].flatMap((mod) => [["x", [mod]], ["v", [mod]], ["n", [mod]], ["n", [mod, "shift"]]])]) {
      await key(code, modifiers);
    }
    assert.equal(await check(() => document.querySelectorAll('[role="tree"] input').length), 0, "Rename must stay unavailable");
    assert.deepEqual(await check(() => window.worktreeFixture.engines.flatMap((fx) =>
      ["create", "rename", "delete", "move", "copy", "copyFromPath"].flatMap((name) => fx.calls[name]))), [], "Tree must never mutate the worktree");
    await click(row(3), "right");
    await menu();
    await key("Escape");

    // A delayed menu/IPC action from generation 1 cannot act on generation 2.
    session = { ...session, root: secondRoot, generation: 2 };
    await click(row(4));
    await key("c", ["control"]);
    await until(() => check(() => window.worktreeFixture.receipts.at(-1)?.includes("worktree has changed")), "Stale IPC failure was not reported");
    assert.equal(clipboard.readText(), "normal text copy");
    await check((next) => window.worktreeFixture.switchRoot(next, 2), secondRoot);
    await pause(80);
    await key("c", ["control"]);
    assert.equal(clipboard.readText(), "normal text copy", "Selection must reset on project switch");
    for (const id of [1, 2]) {
      if (await check((s) => document.querySelector(s)?.getAttribute("aria-expanded") !== "true", row(id))) await click(row(id));
    }
    await click(row(3));
    await key("c", ["control"]);
    await copied(path.join(secondRoot, "folder/file ü.txt"));
    await check(() => window.worktreeFixture.removeFile());
    await pause(80);
    await key("c", ["control"]);
    assert.equal(clipboard.readText(), path.join(secondRoot, "folder/file ü.txt"));
    assert.deepEqual(errors, [], "Renderer errors");
    console.log(JSON.stringify({ result: "PASS", checked: ["Ctrl+C", "Cmd+C", "native Edit > Copy", "multiple paths", "right-click selection", "two-item menu",
      "keyboard menu", "Finder IPC", "empty selection", "stale tree", "native text copy", "read-only keys", "project switch", "removed entry"], screenshots }, null, 2));
  } catch (error) {
    failed = true;
    console.error(error);
    if (errors.length) console.error(errors);
  } finally {
    clearInterval(pulse);
    clearTimeout(deadline);
    if (clipboardBackup && lastClipboardWrite !== undefined && clipboard.readText() === lastClipboardWrite) clipboard.write(clipboardBackup);
    win?.destroy();
    await server?.close();
    app.exit(failed ? 1 : 0);
  }
}
void run();
