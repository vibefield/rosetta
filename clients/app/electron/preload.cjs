// The room's one bridge out of the web world, and it is read-only: git facts
// for the open worktree root, the act of pointing the worktree column at a
// project, and the engine MessagePort relay. Nothing here can write to disk.
const { contextBridge, ipcRenderer } = require("electron");
const os = require("node:os");

// MessagePort cannot pass through contextBridge (the prototype is stripped);
// forwarding through window.postMessage keeps the port object intact — the
// pattern mille's embedding guide prescribes.
ipcRenderer.on("ah-worktree-port", (event, payload) => {
  window.postMessage({ type: "ah-worktree-port", root: payload.root }, "*", event.ports);
});

contextBridge.exposeInMainWorld("agentHistoryHost", {
  /** The user's home directory, so paths can set as `~/…` the way the design
   * record writes them. The daemon cannot answer this: it reports agent data
   * roots already shortened to `~/.claude`, which names no home. */
  homeDir: os.homedir(),
  /** Point the worktree column's engine at a project root. */
  openWorktree: (root) => ipcRenderer.invoke("ah-worktree-open", root),
  /** Branch · dirty count · worktrees for the open worktree root, or null. */
  gitFacts: (root) => ipcRenderer.invoke("ah-worktree-git-facts", root),
});
