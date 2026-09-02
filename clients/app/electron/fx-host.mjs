// The mille engine, in an Electron UtilityProcess — the native walker,
// watcher, and ignore-parser never enter the renderer, which talks to it
// over a transferred MessagePort (the architecture mille's EMBEDDING_UI.md
// prescribes). One process per project root: the main process kills and
// re-forks this on every project switch so the old root's watchers release.
//
// Git decorations register HERE, against the host's decoration store, so
// the shell git client (node:child_process) stays out of the renderer and
// badges fan out to the attached port session automatically.
import { createFileExplorerHost } from "@vibecook/mille/host";
import { registerGitDecorations } from "@vibecook/mille-ui/git";
import { createShellGitClient } from "@vibecook/mille-ui/git/node";
import { createGitService } from "./git-service.mjs";
import { createWorktreeExplorerOptions } from "./worktree-explorer-config.mjs";

/** Electron's MessagePortMain has worker_threads-adjacent but not identical
 * semantics; normalize the two quirks mille's adapter does not expect
 * (MessageEvent wrapping, and postMessage dropping frames when handed an
 * explicit `undefined` transfer list). */
function wrapMessagePortMain(port) {
  return {
    addEventListener: (_type, listener) => {
      port.on("message", (evt) => listener({ data: evt.data }));
    },
    removeEventListener: () => {},
    postMessage: (msg, transfer) => {
      if (transfer && transfer.length > 0) port.postMessage(msg, transfer);
      else port.postMessage(msg);
    },
    start: () => port.start(),
    close: () => port.close(),
  };
}

let host = null;

/**
 * Serve the git column over its own port.
 *
 * A second port rather than a second use of mille's: that one carries
 * mille's own framed protocol and is not ours to multiplex. This one is a
 * plain request/response channel — `{ id, op, args }` in, `{ id, ok, value }`
 * or `{ id, ok: false, error }` out — and every failure crosses it as a
 * value, because a git command that failed is a fact the column states, not
 * an exception the renderer has to survive.
 */
function serveGit(port, service) {
  port.addEventListener("message", (event) => {
    const request = event.data;
    if (request === null || typeof request !== "object" || typeof request.id !== "number") {
      return;
    }
    const run = service[request.op];
    if (typeof run !== "function") {
      port.postMessage({ id: request.id, ok: false, error: `unknown op: ${request.op}` });
      return;
    }
    void Promise.resolve()
      .then(() => run(request.args))
      .then((value) => port.postMessage({ id: request.id, ok: true, value: value ?? null }))
      .catch((error) => {
        port.postMessage({
          id: request.id,
          ok: false,
          error: error?.message ? String(error.message) : "the git command failed",
        });
      });
  });
  port.start();
}

async function bootstrap() {
  const root = process.env.AH_WORKTREE_ROOT;
  if (!root) throw new Error("AH_WORKTREE_ROOT not set");

  host = await createFileExplorerHost(createWorktreeExplorerOptions(root));

  // Attach handler installs before anything slow so the renderer's
  // handshake is never blocked behind a walk.
  const gitService = createGitService(root);

  process.parentPort.on("message", (evt) => {
    const port = evt.ports[0];
    if (!port) return;
    if (evt.data?.type === "attach") host.attachPort(wrapMessagePortMain(port));
    else if (evt.data?.type === "attach-git") serveGit(wrapMessagePortMain(port), gitService);
  });

  try {
    const currentHost = host;
    registerGitDecorations({
      fx: host.local,
      client: createShellGitClient({ rootPath: root }),
      rootPath: root,
      // Letters on files only. A folder roll-up (`M*`) answers "is anything
      // dirty below here", which the column head already answers once for
      // the whole worktree — and it answers it in a second place, in a
      // second colour, on every ancestor of every edit. The design record's
      // rail decorates the file that changed and nothing else.
      propagateToParent: false,
      // The host's store, not host.local's — only the host store fans
      // decorations out to attached port sessions.
      registrar: (provider) => currentHost.registerDecorationProvider(provider),
    });
  } catch (err) {
    // A project without git is a project without letters, not a failure.
    console.warn("[ah-fx-host] git decorations unavailable:", err?.message ?? err);
  }

  process.parentPort.postMessage({ type: "ready" });
}

bootstrap().catch((err) => {
  console.error("[ah-fx-host] bootstrap failed:", err);
  process.exit(1);
});

process.on("exit", () => {
  if (host) void host.dispose();
});
