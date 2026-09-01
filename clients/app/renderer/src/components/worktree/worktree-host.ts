// One transactional renderer connection for column ③. Main transfers the
// mille and git ports together with the root and utility-process generation;
// this store publishes them together only after mille's async handshake has
// completed. A late handshake can therefore never pair an old tree with a
// new git client (or win after a newer project switch).

import { useEffect, useState, useSyncExternalStore } from "react";
import { connectFileExplorer } from "@vibecook/mille/port";
import { connectGitClient } from "../git/git-host";
import type { GitConnection } from "../git/git-host";
import { desktopHost } from "../../lib/desktop-host";
import type { GitFacts } from "../../lib/desktop-host";
import type { WorktreeEngine } from "./types";

/** How much of the tree a port client mirrors before it starts evicting. */
const MIRROR_CAP = 20_000;

/** How often the column re-reads branch and dirty count. Slow on purpose:
 * these are ambient facts about a repository someone else is editing, and
 * the room is a reader. */
const GIT_FACTS_POLL_MS = 15_000;

interface WorktreePortsMessage {
  readonly root: string;
  readonly generation: number;
  readonly treePort: MessagePort;
  readonly gitPort: MessagePort;
}

interface WorktreeStatusMessage {
  readonly root: string;
  readonly generation: number;
  readonly state: "starting" | "error";
  readonly message?: string;
}

export interface WorktreeConnection {
  readonly root: string;
  readonly generation: number;
  readonly engine: WorktreeEngine;
  readonly git: GitConnection;
  dispose(): void;
}

export type WorktreeConnectionState =
  | { readonly status: "idle" }
  | { readonly status: "connecting"; readonly root: string; readonly generation: number }
  | { readonly status: "ready"; readonly connection: WorktreeConnection }
  | {
      readonly status: "error";
      readonly root: string;
      readonly generation: number;
      readonly message: string;
    };

type StoreListener = () => void;

const storeListeners = new Set<StoreListener>();
let storeState: WorktreeConnectionState = { status: "idle" };
let current: WorktreeConnection | null = null;
let latestGeneration = 0;
let latestRoot: string | null = null;
let handshakeAttempt = 0;
let pendingHandshake: {
  readonly attempt: number;
  readonly treePort: MessagePort;
  readonly git: GitConnection;
} | null = null;

function publish(next: WorktreeConnectionState): void {
  storeState = next;
  for (const listener of storeListeners) listener();
}

function subscribe(listener: StoreListener): () => void {
  storeListeners.add(listener);
  return () => storeListeners.delete(listener);
}

function getSnapshot(): WorktreeConnectionState {
  return storeState;
}

function closePorts(message: WorktreePortsMessage): void {
  message.treePort.close();
  message.gitPort.close();
}

function disposeCurrent(): void {
  const connection = current;
  current = null;
  connection?.dispose();
}

function cancelPendingHandshake(): void {
  const pending = pendingHandshake;
  pendingHandshake = null;
  if (pending === null) return;
  pending.treePort.close();
  pending.git.dispose();
}

function acceptStatus(message: WorktreeStatusMessage): void {
  if (message.generation < latestGeneration) return;
  if (
    message.generation === latestGeneration &&
    latestRoot !== null &&
    message.root !== latestRoot
  ) {
    return;
  }
  if (
    message.state === "starting" &&
    ((storeState.status === "ready" &&
      storeState.connection.generation === message.generation) ||
      (storeState.status === "connecting" && storeState.generation === message.generation))
  ) {
    return;
  }

  latestGeneration = message.generation;
  latestRoot = message.root;
  handshakeAttempt += 1;
  cancelPendingHandshake();
  disposeCurrent();

  if (message.state === "starting") {
    publish({
      status: "connecting",
      root: message.root,
      generation: message.generation,
    });
    return;
  }

  publish({
    status: "error",
    root: message.root,
    generation: message.generation,
    message: message.message ?? "The worktree process stopped unexpectedly.",
  });
}

function acceptPorts(message: WorktreePortsMessage): void {
  if (message.generation < latestGeneration) {
    closePorts(message);
    return;
  }
  if (
    message.generation === latestGeneration &&
    latestRoot !== null &&
    message.root !== latestRoot
  ) {
    closePorts(message);
    return;
  }
  if (
    storeState.status === "ready" &&
    storeState.connection.generation === message.generation
  ) {
    closePorts(message);
    return;
  }
  if (storeState.status === "error" && storeState.generation === message.generation) {
    closePorts(message);
    return;
  }

  if (message.generation > latestGeneration) {
    latestGeneration = message.generation;
    latestRoot = message.root;
    disposeCurrent();
  }
  publish({
    status: "connecting",
    root: message.root,
    generation: message.generation,
  });

  cancelPendingHandshake();
  const attempt = (handshakeAttempt += 1);
  const git = connectGitClient(message.gitPort, message.root, message.generation);
  pendingHandshake = { attempt, treePort: message.treePort, git };
  void connectFileExplorer(message.treePort, { mirrorCap: MIRROR_CAP })
    .then((fx) => {
      if (pendingHandshake?.attempt === attempt) pendingHandshake = null;
      if (attempt !== handshakeAttempt || message.generation !== latestGeneration) {
        void fx.dispose();
        git.dispose();
        return;
      }

      let disposed = false;
      const engine: WorktreeEngine = {
        fx,
        root: message.root,
        generation: message.generation,
        dispose: () => void fx.dispose(),
      };
      const connection: WorktreeConnection = {
        root: message.root,
        generation: message.generation,
        engine,
        git,
        dispose: () => {
          if (disposed) return;
          disposed = true;
          engine.dispose();
          git.dispose();
        },
      };
      disposeCurrent();
      current = connection;
      publish({ status: "ready", connection });
    })
    .catch((error: unknown) => {
      if (pendingHandshake?.attempt === attempt) pendingHandshake = null;
      git.dispose();
      message.treePort.close();
      if (attempt !== handshakeAttempt || message.generation !== latestGeneration) return;
      publish({
        status: "error",
        root: message.root,
        generation: message.generation,
        message: error instanceof Error ? error.message : "Could not connect to the worktree.",
      });
    });
}

// Installed at module load rather than in a React effect: transferred ports
// are one-owner capabilities and must survive a Strict Mode setup/cleanup
// cycle or an arrival before the column mounts.
window.addEventListener("message", (event: MessageEvent) => {
  const data = event.data as
    | {
        readonly type?: string;
        readonly root?: string;
        readonly generation?: number;
        readonly state?: string;
        readonly message?: string;
      }
    | undefined;
  if (
    data?.type === "ah-worktree-ports" &&
    typeof data.root === "string" &&
    Number.isSafeInteger(data.generation) &&
    (data.generation ?? 0) > 0 &&
    event.ports.length === 2
  ) {
    acceptPorts({
      root: data.root,
      generation: data.generation as number,
      treePort: event.ports[0],
      gitPort: event.ports[1],
    });
    return;
  }
  if (
    data?.type === "ah-worktree-status" &&
    typeof data.root === "string" &&
    Number.isSafeInteger(data.generation) &&
    (data.generation ?? 0) > 0 &&
    (data.state === "starting" || data.state === "error")
  ) {
    acceptStatus({
      root: data.root,
      generation: data.generation as number,
      state: data.state,
      ...(typeof data.message === "string" ? { message: data.message } : {}),
    });
  }
});

/** The current generation-safe, atomically paired tree + git connection. */
export function useWorktreeConnection(): WorktreeConnectionState {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/** Branch, dirty count and sibling worktrees for one exact connection
 * generation. `null` until the first answer, and for a folder without git. */
export function useGitFacts(root: string | null, generation: number | null): GitFacts | null {
  const [facts, setFacts] = useState<GitFacts | null>(null);
  useEffect(() => {
    setFacts(null);
    const host = desktopHost();
    if (root === null || generation === null || host === null) return;

    let cancelled = false;
    const read = () => {
      void host
        .gitFacts(root, generation)
        .then((next) => {
          if (!cancelled) setFacts(next);
        })
        .catch(() => {
          if (!cancelled) setFacts(null);
        });
    };

    read();
    const timer = window.setInterval(read, GIT_FACTS_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [root, generation]);
  return facts;
}
