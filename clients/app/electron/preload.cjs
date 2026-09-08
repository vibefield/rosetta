// The room's one bridge out of the web world: git facts for the open worktree
// root, the act of pointing the worktree column at a project, the engine
// MessagePort relay, and handing one viewed document to the browser. Nothing
// here chooses a destination or writes directly, and nothing here decides
// anything — every argument is re-validated by the main process.
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
  daemonState: () => ipcRenderer.invoke("ah-daemon-state"),
  onDaemonState: (listener) => {
    const handler = (_event, state) => listener(state);
    ipcRenderer.on("ah-daemon-state", handler);
    return () => ipcRenderer.removeListener("ah-daemon-state", handler);
  },
  /** The user's home directory, so paths can set as `~/…` the way the design
   * record writes them. The daemon cannot answer this: it reports agent data
   * roots already shortened to `~/.claude`, which names no home. */
  homeDir: os.homedir(),
  /** Point the worktree column's engine at a project root. */
  openWorktree: (root) => ipcRenderer.invoke("ah-worktree-open", root),
  /** Branch · dirty count · worktrees for the open worktree root, or null. */
  gitFacts: (root, generation) =>
    ipcRenderer.invoke("ah-worktree-git-facts", root, generation),
  /** Open the exact HTML/SVG bytes shown by the viewer in the default
   * browser. Main validates and materializes its own private snapshot. */
  openInBrowser: (name, bytes) =>
    ipcRenderer.invoke("ah-file-open-browser-preview", { name, bytes }),
  /** One site's own icon as a `data:` URL, or null. Main validates the host
   * and fetches from the site itself — never a favicon service. */
  favicon: (host) => ipcRenderer.invoke("ah-favicon", host),
});
