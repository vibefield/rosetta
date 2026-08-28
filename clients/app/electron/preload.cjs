// The room's one bridge out of the web world, and it is read-only: git facts
// for the open rail root, the act of pointing the rail at a project, and the
// engine MessagePort relay. Nothing here can write to disk.
const { contextBridge, ipcRenderer } = require("electron");

// MessagePort cannot pass through contextBridge (the prototype is stripped);
// forwarding through window.postMessage keeps the port object intact — the
// pattern mille's embedding guide prescribes.
ipcRenderer.on("ah-rail-port", (event, payload) => {
  window.postMessage({ type: "ah-rail-port", root: payload.root }, "*", event.ports);
});

contextBridge.exposeInMainWorld("agentHistoryHost", {
  /** Point the worktree rail's engine at a project root. */
  openRail: (root) => ipcRenderer.invoke("ah-rail-open", root),
  /** Branch · dirty count · worktrees for the open rail root, or null. */
  gitFacts: (root) => ipcRenderer.invoke("ah-rail-git-facts", root),
});
