// The room's one bridge out of the web world: git facts for the open worktree
// root, the act of pointing the worktree column at a project, the engine
// MessagePort relay, and handing one HTML file to the browser. Nothing here
// can write to disk, and nothing here decides anything — every argument is
// re-validated by the main process, which is the only side that knows which
// root is open.
const { contextBridge, ipcRenderer } = require("electron");
const os = require("node:os");

// MessagePort cannot pass through contextBridge (the prototype is stripped);
// forwarding through window.postMessage keeps the port object intact — the
// pattern mille's embedding guide prescribes.
ipcRenderer.on("ah-worktree-ports", (event, payload) => {
  window.postMessage(
    {
      type: "ah-worktree-ports",
      root: payload.root,
      generation: payload.generation,
    },
    "*",
    event.ports,
  );
});

ipcRenderer.on("ah-worktree-status", (_event, payload) => {
  window.postMessage({ ...payload, type: "ah-worktree-status" }, "*");
});

contextBridge.exposeInMainWorld("agentHistoryHost", {
  /** The user's home directory, so paths can set as `~/…` the way the design
   * record writes them. The daemon cannot answer this: it reports agent data
   * roots already shortened to `~/.claude`, which names no home. */
  homeDir: os.homedir(),
  /** Point the worktree column's engine at a project root. */
  openWorktree: (root) => ipcRenderer.invoke("ah-worktree-open", root),
  /** Branch · dirty count · worktrees for the open worktree root, or null. */
  gitFacts: (root, generation) =>
    ipcRenderer.invoke("ah-worktree-git-facts", root, generation),
  /** Open one HTML file from the open worktree in the default browser.
   * Resolves false whenever main refuses — a wrong extension, a path outside
   * the root, a file that has since gone. */
  openInBrowser: (path) => ipcRenderer.invoke("ah-file-open-external", path),
});
