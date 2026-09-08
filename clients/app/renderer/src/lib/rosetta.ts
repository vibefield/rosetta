// One client for the whole room. The daemon address is localhost by
// default; `localStorage.setItem("ah.daemon", "http://127.0.0.1:PORT")`
// repoints a dev build without a rebuild.
import { desktopHost } from "./desktop-host";
import type { DesktopDaemonState } from "./desktop-host";
import { RosettaClient } from "@rosetta/protocol";

function baseUrl(): string {
  const fromQuery = new URLSearchParams(window.location.search).get("daemon");
  if (fromQuery) return fromQuery;
  try {
    const stored = localStorage.getItem("ah.daemon");
    if (stored) return stored;
  } catch {
    // storage unavailable; use the default
  }
  return "http://127.0.0.1:4640";
}

export const client = new RosettaClient({ baseUrl: baseUrl() });

let daemonState: DesktopDaemonState | null = null;
const listeners = new Set<() => void>();
export function desktopDaemonState(): DesktopDaemonState | null {
  return daemonState;
}
export function onDesktopDaemonState(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
function acceptDaemonState(state: DesktopDaemonState): void {
  if (daemonState !== null && state.revision <= daemonState.revision) return;
  daemonState = state;
  // An unavailable owned daemon must never fall through to somebody else's
  // service at the development default port.
  client.setBaseUrl(state.origin ?? "http://127.0.0.1:0");
  for (const listener of listeners) listener();
}
const host = desktopHost();
const removeDaemonListener = host?.onDaemonState?.(acceptDaemonState);
void host
  ?.daemonState?.()
  .then(acceptDaemonState)
  .catch(() => {});
window.addEventListener("pagehide", () => removeDaemonListener?.(), { once: true });
