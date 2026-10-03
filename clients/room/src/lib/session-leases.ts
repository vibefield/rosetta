// The room's half of the session port: one open per session, however many
// parts of the room read it, and one close when the last of them lets go. A
// host whose port mints a credential per open therefore mints one per session
// the reader is actually looking at, not one per component.
import type { SessionClient, SessionPort } from "../ports";

export interface SessionLease {
  /** The client, once the port has answered; null while it opens. */
  readonly client: SessionClient | null;
  /** Settles with the client, or rejects with the port's refusal. */
  readonly ready: Promise<SessionClient>;
  /** Give the session back. Idempotent; the last holder's release closes it. */
  release(): void;
}

export interface SessionLeases {
  acquire(sessionId: string): SessionLease;
  /** One read through a session's client, held only for the read. */
  run<T>(sessionId: string, read: (client: SessionClient) => Promise<T>): Promise<T>;
}

interface OpenSession {
  holders: number;
  client: SessionClient | null;
  readonly ready: Promise<SessionClient>;
}

function isPromiseLike<T>(value: T | PromiseLike<T>): value is PromiseLike<T> {
  return typeof (value as { then?: unknown } | null)?.then === "function";
}

export function createSessionLeases(port: SessionPort): SessionLeases {
  const open = new Map<string, OpenSession>();

  function openSession(sessionId: string): OpenSession {
    let answered: SessionClient | PromiseLike<SessionClient>;
    try {
      answered = port.open(sessionId);
    } catch (error) {
      answered = Promise.reject(error);
    }
    if (!isPromiseLike(answered)) {
      return { holders: 0, client: answered, ready: Promise.resolve(answered) };
    }
    const session: OpenSession = { holders: 0, client: null, ready: Promise.resolve(answered) };
    session.ready.then(
      (client) => {
        session.client = client;
      },
      // A refusal is the reader's to see through `ready`; nothing to keep here.
      () => {},
    );
    return session;
  }

  function acquire(sessionId: string): SessionLease {
    let session = open.get(sessionId);
    if (session === undefined) {
      session = openSession(sessionId);
      open.set(sessionId, session);
    }
    session.holders += 1;
    const held = session;
    let released = false;
    return {
      get client() {
        return held.client;
      },
      ready: held.ready,
      release() {
        if (released) return;
        released = true;
        held.holders -= 1;
        if (held.holders === 0 && open.get(sessionId) === held) {
          open.delete(sessionId);
          port.close(sessionId);
        }
      },
    };
  }

  async function run<T>(sessionId: string, read: (client: SessionClient) => Promise<T>): Promise<T> {
    const lease = acquire(sessionId);
    try {
      return await read(lease.client ?? (await lease.ready));
    } finally {
      lease.release();
    }
  }

  return { acquire, run };
}
