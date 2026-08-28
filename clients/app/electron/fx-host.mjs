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

async function bootstrap() {
  const root = process.env.AH_RAIL_ROOT;
  if (!root) throw new Error("AH_RAIL_ROOT not set");

  host = await createFileExplorerHost({
    roots: [root],
    respectIgnore: true,
    followSymlinks: "smart",
    watchDebounceMs: 75,
    // Lazy expansion: seed the root, prefetch children on expand. A history
    // room's rail must never walk a monorepo upfront.
    initialWalk: "roots-only",
  });

  // Attach handler installs before anything slow so the renderer's
  // handshake is never blocked behind a walk.
  process.parentPort.on("message", (evt) => {
    const port = evt.ports[0];
    if (evt.data?.type === "attach" && port) {
      host.attachPort(wrapMessagePortMain(port));
    }
  });

  try {
    const currentHost = host;
    registerGitDecorations({
      fx: host.local,
      client: createShellGitClient({ rootPath: root }),
      rootPath: root,
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
