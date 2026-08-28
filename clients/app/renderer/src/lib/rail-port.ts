// The rail's engine port, received from the preload via window.postMessage
// (a MessagePort cannot survive contextBridge). Subscribed at module load —
// outside React — so a port posted before the first effect runs is never
// lost, and StrictMode's double-invoked effects never race the handshake.
// Every project switch produces a fresh port; listeners swap connections.

export interface RailPortMessage {
  port: MessagePort;
  /** The project root the engine behind this port is walking. */
  root: string;
}

type Listener = (msg: RailPortMessage) => void;
const listeners = new Set<Listener>();
let latest: RailPortMessage | null = null;

window.addEventListener("message", (evt: MessageEvent) => {
  const data = evt.data as { type?: string; root?: string } | undefined;
  if (data?.type !== "ah-rail-port" || evt.ports.length === 0 || !data.root) return;
  latest = { port: evt.ports[0], root: data.root };
  for (const cb of listeners) cb(latest);
});

/** Subscribe to port arrivals; fires immediately with the latest unconsumed
 * port if one already landed. Returns a disposer. */
export function onRailPort(cb: Listener): () => void {
  listeners.add(cb);
  if (latest !== null) cb(latest);
  return () => {
    listeners.delete(cb);
  };
}

/** Consume the pending port so a later subscriber never connects to a port
 * an earlier connection already claimed. */
export function takeRailPort(msg: RailPortMessage): void {
  if (latest === msg) latest = null;
}

export interface GitFacts {
  branch: string | null;
  dirty: number | null;
  worktrees: { path: string; branch: string | null }[];
}

interface AgentHistoryHost {
  /** The user's home directory — absent in a browser tab. */
  homeDir?: string;
  openRail(root: string): Promise<boolean>;
  gitFacts(root: string): Promise<GitFacts | null>;
}

/** The Electron preload's bridge, absent when the room runs in a plain
 * browser tab — the rail then states that honestly instead of pretending. */
export function host(): AgentHistoryHost | null {
  return (window as { agentHistoryHost?: AgentHistoryHost }).agentHistoryHost ?? null;
}
