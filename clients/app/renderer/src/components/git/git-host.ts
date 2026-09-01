// The renderer's git client over the git half of a paired worktree
// connection. Port ownership and generation arbitration live in
// `worktree-host`; this module only turns one already-accepted port into a
// disposable request/response client.

import { useCallback, useEffect, useState } from "react";
import type { GitClient } from "./types";

/** A read that has not been asked for in this long is asked again. Slow on
 * purpose: this is a repository someone else is editing, and the room is a
 * reader that must not turn into a poller. */
const POLL_MS = 4_000;

/** A `GitClient` over one port. Requests are correlated by id; the port has a
 * single consumer, so the id space is local and monotonic. */
function connect(port: MessagePort): { readonly client: GitClient; dispose(): void } {
  let nextId = 0;
  let disposed = false;
  const waiting = new Map<
    number,
    {
      readonly resolve: (value: unknown) => void;
      readonly reject: (error: Error) => void;
    }
  >();

  const onMessage = (event: MessageEvent) => {
    const reply = event.data as { id?: number; ok?: boolean; value?: unknown; error?: string };
    if (typeof reply?.id !== "number") return;
    const settle = waiting.get(reply.id);
    if (settle === undefined) return;
    waiting.delete(reply.id);
    if (reply.ok === true) settle.resolve(reply.value);
    else settle.reject(new Error(reply.error ?? "the git command failed"));
  };
  const rejectWaiting = (message: string) => {
    const error = new Error(message);
    for (const request of waiting.values()) request.reject(error);
    waiting.clear();
  };
  const onMessageError = () => rejectWaiting("the git connection closed");
  port.addEventListener("message", onMessage);
  port.addEventListener("messageerror", onMessageError);
  port.start();

  const call = <T,>(op: string, args?: unknown): Promise<T> =>
    new Promise<T>((resolve, reject) => {
      if (disposed) {
        reject(new Error("the git connection closed"));
        return;
      }
      const id = (nextId += 1);
      waiting.set(id, {
        resolve: (value) => resolve(value as T),
        reject,
      });
      try {
        port.postMessage({ id, op, args });
      } catch (error) {
        waiting.delete(id);
        reject(error instanceof Error ? error : new Error("the git connection closed"));
      }
    });

  const client: GitClient = {
    branch: () => call("branch"),
    status: () => call("status"),
    log: () => call("log"),
    diff: (request) => call("diff", request),
    commitFiles: (sha) => call("commitFiles", sha),
    stage: (paths) => call("stage", paths),
    unstage: (paths) => call("unstage", paths),
    discard: (paths) => call("discard", paths),
  };

  return {
    client,
    dispose: () => {
      if (disposed) return;
      disposed = true;
      port.removeEventListener("message", onMessage);
      port.removeEventListener("messageerror", onMessageError);
      rejectWaiting("the git connection closed");
      port.close();
    },
  };
}

export interface GitConnection {
  readonly client: GitClient;
  /** The root the process behind this client is serving. */
  readonly root: string;
  readonly generation: number;
  dispose(): void;
}

/** Connect the git half only after `worktree-host` has accepted the paired
 * generation. Disposal closes the port and rejects every pending request. */
export function connectGitClient(
  port: MessagePort,
  root: string,
  generation: number,
): GitConnection {
  const connected = connect(port);
  return {
    client: connected.client,
    root,
    generation,
    dispose: connected.dispose,
  };
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
