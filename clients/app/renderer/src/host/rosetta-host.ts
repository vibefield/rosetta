// rosetta's app as the History room's host: one client, pointed at the daemon
// this app owns (or the one a developer names), and the room's ports over it.
// The address comes from `?daemon=`, then `localStorage["ah.daemon"]`, then
// the development default; the Electron shell's daemon-state pushes repoint it
// when the app owns its daemon.
import { RosettaClient } from "@rosetta/protocol";
import type { AssetRef } from "@rosetta/protocol";
import { desktopHost } from "./desktop-bridge";
import type { DesktopDaemonState, DesktopHost } from "./desktop-bridge";
import { worktreePorts } from "./worktree-ports";
import type { AssetSource, CatalogPort, HostPort, RoomPorts, SessionPort } from "../ports";

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

export interface RosettaHost {
  readonly client: RosettaClient;
  readonly ports: RoomPorts;
  /** The shell's latest word on the daemon it owns, or null in a browser tab. */
  daemonState(): DesktopDaemonState | null;
  onDaemonState(listener: () => void): () => void;
}

/** The daemon's own URL for a stored member, as long as it names that daemon. */
export function sameOriginAssetUrl(client: RosettaClient, asset: AssetRef): string | null {
  try {
    const url = new URL(asset.url, `${client.baseUrl}/`);
    return (url.protocol === "http:" || url.protocol === "https:") && url.origin === new URL(client.baseUrl).origin
      ? url.toString()
      : null;
  } catch {
    return null;
  }
}

/** The room's desktop, over the Electron bridge. */
export function hostPortOver(bridge: DesktopHost): HostPort {
  return {
    homeDir: bridge.homeDir ?? null,
    platform: bridge.platform ?? null,
    openInBrowser: (name, bytes) => bridge.openInBrowser(name, bytes),
    // A shell whose preload predates the method answers every host with the
    // globe, as before, rather than failing the line.
    favicon: (host) =>
      typeof bridge.favicon === "function" ? bridge.favicon(host) : Promise.resolve(null),
    worktree: {
      open: (root) => bridge.openWorktree(root),
      gitFacts: (root, generation) => bridge.gitFacts(root, generation),
      fileAction: (request) => bridge.worktreeFileAction(request),
      ports: worktreePorts,
    },
  };
}

export function createRosettaHost(): RosettaHost {
  const client = new RosettaClient({ baseUrl: baseUrl() });

  let daemonState: DesktopDaemonState | null = null;
  const listeners = new Set<() => void>();
  const onDaemonState = (listener: () => void) => {
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  };
  const acceptDaemonState = (state: DesktopDaemonState): void => {
    if (daemonState !== null && state.revision <= daemonState.revision) return;
    daemonState = state;
    // An unavailable owned daemon must never fall through to somebody else's
    // service at the development default port.
    client.setBaseUrl(state.origin ?? "http://127.0.0.1:0");
    for (const listener of listeners) listener();
  };
  const bridge = desktopHost();
  const removeDaemonListener = bridge?.onDaemonState?.(acceptDaemonState);
  void bridge
    ?.daemonState?.()
    .then(acceptDaemonState)
    .catch(() => {});
  window.addEventListener("pagehide", () => removeDaemonListener?.(), { once: true });

  const catalog: CatalogPort = {
    subscribeMeta: (handlers) => client.subscribeMeta(handlers),
    status: (signal) => client.status(signal),
    onConnectionChange: (listener) => {
      const removeConnection = client.onConnectionChange(listener);
      const removeDesktop = onDaemonState(listener);
      return () => {
        removeConnection();
        removeDesktop();
      };
    },
    unavailable: () => daemonState?.error ?? null,
    heatmap: (projectId, options, signal) => client.heatmap(projectId, options, signal),
    contribution: (projectId, signal) => client.contribution(projectId, signal),
    usage: (projectId, options, signal) => client.usage(projectId, options, signal),
    dayUsage: (projectId, day, offsetMinutes, signal) => client.dayUsage(projectId, day, offsetMinutes, signal),
    search: (request, signal) => client.search(request, signal),
    boost: (sessionId, signal) => client.boost(sessionId, signal),
  };
  // One client reads every session: this daemon wants no per-session
  // credential, so opening is handing it over and closing is nothing.
  const sessions: SessionPort = { open: () => client, close: () => {} };
  const assets: AssetSource = {
    imageAssetUrl: (asset, variant) => client.imageAssetUrl(asset, variant),
    attachmentAssetUrl: (asset) => sameOriginAssetUrl(client, asset),
  };
  return {
    client,
    ports: { catalog, sessions, assets, host: bridge === null ? null : hostPortOver(bridge) },
    daemonState: () => daemonState,
    onDaemonState,
  };
}
