// The renderer's half of the git channel.
//
// Same shape as `worktree-host`, and for the same reason: the port arrives
// through `window.postMessage` (a MessagePort cannot survive contextBridge)
// and may land before the first effect runs, so the subscription is set up at
// module load and outside React.

import { useCallback, useEffect, useState } from "react";
import type { GitClient } from "./types";

/** A read that has not been asked for in this long is asked again. Slow on
 * purpose: this is a repository someone else is editing, and the room is a
 * reader that must not turn into a poller. */
const POLL_MS = 4_000;

interface GitPortMessage {
  readonly port: MessagePort;
  readonly root: string;
}

type PortListener = (message: GitPortMessage) => void;

const listeners = new Set<PortListener>();
let pending: GitPortMessage | null = null;

window.addEventListener("message", (event: MessageEvent) => {
  const data = event.data as { type?: string; root?: string } | undefined;
  if (data?.type !== "ah-git-port" || event.ports.length === 0 || !data.root) return;
  pending = { port: event.ports[0], root: data.root };
  for (const listener of listeners) listener(pending);
});

function onGitPort(listener: PortListener): () => void {
  listeners.add(listener);
  if (pending !== null) listener(pending);
  return () => {
    listeners.delete(listener);
  };
}

/** A `GitClient` over one port. Requests are correlated by id; the port has a
 * single consumer, so the id space is local and monotonic. */
function connect(port: MessagePort): GitClient {
  let nextId = 0;
  const waiting = new Map<number, (result: { ok: boolean; value?: unknown; error?: string }) => void>();

  port.addEventListener("message", (event: MessageEvent) => {
    const reply = event.data as { id?: number; ok?: boolean; value?: unknown; error?: string };
    if (typeof reply?.id !== "number") return;
    const settle = waiting.get(reply.id);
    if (settle === undefined) return;
    waiting.delete(reply.id);
    settle({ ok: reply.ok === true, value: reply.value, error: reply.error });
  });
  port.start();

  const call = <T,>(op: string, args?: unknown): Promise<T> =>
    new Promise<T>((resolve, reject) => {
      const id = (nextId += 1);
      waiting.set(id, (result) => {
        if (result.ok) resolve(result.value as T);
        else reject(new Error(result.error ?? "the git command failed"));
      });
      port.postMessage({ id, op, args });
    });

  return {
    branch: () => call("branch"),
    status: () => call("status"),
    log: () => call("log"),
    diff: (request) => call("diff", request),
    commitFiles: (sha) => call("commitFiles", sha),
    stage: (paths) => call("stage", paths),
    unstage: (paths) => call("unstage", paths),
    discard: (paths) => call("discard", paths),
  };
}

export interface GitConnection {
  readonly client: GitClient;
  /** The root the process behind this client is serving. */
  readonly root: string;
}

/** The one live git connection, swapped whenever a project switch forks a
 * fresh engine. `null` outside the desktop shell, and until the first port. */
export function useGitClient(): GitConnection | null {
  const [connection, setConnection] = useState<GitConnection | null>(null);
  useEffect(
    () =>
      onGitPort((message) => {
        setConnection({ client: connect(message.port), root: message.root });
      }),
    [],
  );
  return connection;
}

export type Refresh = () => void;

/**
 * Poll one read off a client.
 *
 * `read` must be stable. The value is `null` until the first answer, and a
 * failed read keeps the last good value rather than blanking the column —
 * a repository mid-rebase answers nothing for a moment, and that is not a
 * reason to tell the reader their work disappeared.
 */
export function useGitRead<T>(
  connection: GitConnection | null,
  read: (client: GitClient) => Promise<T>,
  refreshKey: number,
): { value: T | null; refresh: Refresh } {
  const [value, setValue] = useState<T | null>(null);
  const [tick, setTick] = useState(0);
  const refresh = useCallback(() => setTick((n) => n + 1), []);

  useEffect(() => {
    if (connection === null) {
      setValue(null);
      return;
    }
    let cancelled = false;
    const run = () => {
      void read(connection.client)
        .then((next) => {
          if (!cancelled) setValue(next);
        })
        .catch(() => {
          // A read that failed is not news the column can act on; the next
          // tick will try again, and the last good answer stays on screen.
        });
    };
    run();
    const timer = window.setInterval(run, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
    // `read` is a stable module-level function per call site.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connection, tick, refreshKey]);

  return { value, refresh };
}
