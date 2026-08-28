// Everything the desktop shell hands column ③: the engine port, and the git
// facts for the root behind it. Two different transports for two different
// kinds of answer — a MessagePort the engine speaks over, and IPC for three
// numbers a shell command produces — so they are read by two hooks that fail
// independently. A repository the git calls cannot answer for still gets a
// tree; a folder with no engine still gets its branch.

import { useEffect, useState } from "react";
import { connectFileExplorer } from "@vibecook/mille/port";
import { desktopHost } from "../../lib/desktop-host";
import type { GitFacts } from "../../lib/desktop-host";
import type { WorktreeEngine } from "./types";

/** How much of the tree a port client mirrors before it starts evicting. */
const MIRROR_CAP = 20_000;

/** How often the column re-reads branch and dirty count. Slow on purpose:
 * these are ambient facts about a repository someone else is editing, and
 * the room is a reader. */
const GIT_FACTS_POLL_MS = 15_000;

interface EnginePortMessage {
  readonly port: MessagePort;
  /** The project root the engine behind this port is walking. */
  readonly root: string;
}

type PortListener = (message: EnginePortMessage) => void;

const portListeners = new Set<PortListener>();
let pendingPort: EnginePortMessage | null = null;

// Subscribed at module load, outside React: the preload forwards the port
// through `window.postMessage` (a MessagePort cannot survive contextBridge),
// and a port that lands before the first effect runs must not be dropped on
// the floor. Every project switch produces a fresh one.
window.addEventListener("message", (event: MessageEvent) => {
  const data = event.data as { type?: string; root?: string } | undefined;
  if (data?.type !== "ah-worktree-port" || event.ports.length === 0 || !data.root) return;
  pendingPort = { port: event.ports[0], root: data.root };
  for (const listener of portListeners) listener(pendingPort);
});

/** Subscribe to port arrivals, firing at once with an unclaimed one that
 * already landed. Returns a disposer. */
function onEnginePort(listener: PortListener): () => void {
  portListeners.add(listener);
  if (pendingPort !== null) listener(pendingPort);
  return () => {
    portListeners.delete(listener);
  };
}

/** Claim a pending port so a later subscriber never connects to one an
 * earlier connection already took — a MessagePort has a single consumer. */
function claimEnginePort(message: EnginePortMessage): void {
  if (pendingPort === message) pendingPort = null;
}

/**
 * The one live engine connection, swapped whenever a fresh port arrives.
 *
 * The subscription is set up once and never re-run: connections are claimed
 * imperatively inside it, so a double-invoked effect would race the
 * handshake for a port only one consumer can have.
 */
export function useWorktreeEngine(): WorktreeEngine | null {
  const [engine, setEngine] = useState<WorktreeEngine | null>(null);
  useEffect(() => {
    let current: WorktreeEngine | null = null;
    let cancelled = false;

    const stop = onEnginePort((message) => {
      claimEnginePort(message);
      void (async () => {
        try {
          const fx = await connectFileExplorer(message.port, { mirrorCap: MIRROR_CAP });
          if (cancelled) {
            void fx.dispose();
            return;
          }
          current?.dispose();
          current = {
            fx,
            root: message.root,
            dispose: () => void fx.dispose(),
          };
          setEngine(current);
        } catch {
          // A dead port — a switch superseded it before the handshake
          // finished. The next arrival wins; there is nothing to report.
        }
      })();
    });

    return () => {
      cancelled = true;
      stop();
      current?.dispose();
    };
  }, []);
  return engine;
}

/** Branch, dirty count and sibling worktrees for `root`, re-read on a slow
 * timer. `null` until the first answer, and for a folder without git. */
export function useGitFacts(root: string | null): GitFacts | null {
  const [facts, setFacts] = useState<GitFacts | null>(null);
  useEffect(() => {
    setFacts(null);
    const host = desktopHost();
    if (root === null || host === null) return;

    let cancelled = false;
    const read = () => {
      void host.gitFacts(root).then((next) => {
        if (!cancelled) setFacts(next);
      });
    };

    read();
    const timer = window.setInterval(read, GIT_FACTS_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [root]);
  return facts;
}
