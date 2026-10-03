// The ports, as the room's parts read them. `HistoryRoom` provides them; a
// harness that mounts one column on its own wraps it in `RoomProvider`.
import { createContext, useContext, useEffect, useMemo, useSyncExternalStore } from "react";
import type { ReactNode } from "react";
import type { AssetSource, CatalogPort, HostPort, RoomPorts } from "./ports";
import { createSessionLeases } from "./lib/session-leases";
import type { SessionLeases } from "./lib/session-leases";
import { createFaviconStore } from "./lib/transcript/favicons";
import type { FaviconStore } from "./lib/transcript/favicons";

/** The two faces the project panel's chart can open on. */
export type SummaryTab = "activity" | "models";

/** What a host decides about the room beyond its ports. */
export interface RoomOptions {
  /** The chart the project panel opens on (rosetta's app reads `?tab=`). */
  readonly initialSummaryTab?: SummaryTab | undefined;
  /** Where the room's dialogs and its sheet are portalled; the document's
   * body when absent. */
  readonly portalContainer?: HTMLElement | null | undefined;
}

/** How many of the room's dialogs are open: the root recedes while any is. */
export interface OverlayCount {
  hold(): () => void;
  subscribe(listener: () => void): () => void;
  count(): number;
}

function createOverlayCount(): OverlayCount {
  let open = 0;
  const listeners = new Set<() => void>();
  const announce = () => {
    for (const listener of listeners) listener();
  };
  return {
    hold() {
      open += 1;
      announce();
      let released = false;
      return () => {
        if (released) return;
        released = true;
        open -= 1;
        announce();
      };
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    count: () => open,
  };
}

export interface Room {
  readonly catalog: CatalogPort;
  readonly sessions: SessionLeases;
  readonly assets: AssetSource;
  readonly host: HostPort | null;
  readonly initialSummaryTab: SummaryTab;
  /** The room's site icons, asked of its host. */
  readonly favicons: FaviconStore;
  readonly portalContainer: HTMLElement | null;
  readonly overlays: OverlayCount;
}

const RoomContext = createContext<Room | null>(null);

export function RoomProvider({
  ports,
  options = {},
  children,
}: {
  readonly ports: RoomPorts;
  readonly options?: RoomOptions;
  readonly children?: ReactNode;
}) {
  const { catalog, sessions: sessionPort, assets } = ports;
  const host = ports.host ?? null;
  const initialSummaryTab = options.initialSummaryTab ?? "activity";
  const portalContainer = options.portalContainer ?? null;
  const overlays = useMemo(createOverlayCount, []);
  const sessions = useMemo(() => createSessionLeases(sessionPort), [sessionPort]);
  const favicons = useMemo(
    () => createFaviconStore(host === null ? null : (site) => host.favicon(site)),
    [host],
  );
  const room = useMemo<Room>(
    () => ({ catalog, sessions, assets, host, initialSummaryTab, favicons, portalContainer, overlays }),
    [catalog, sessions, assets, host, initialSummaryTab, favicons, portalContainer, overlays],
  );
  return <RoomContext.Provider value={room}>{children}</RoomContext.Provider>;
}

export function useRoom(): Room {
  const room = useContext(RoomContext);
  if (room === null) {
    throw new Error("The History room's parts read their ports from <HistoryRoom> or <RoomProvider>.");
  }
  return room;
}

/** Mark the room's root while one of its dialogs is open. A part mounted
 * outside any room (a harness) marks nothing. */
export function useRoomOverlay(open: boolean): void {
  const overlays = useContext(RoomContext)?.overlays ?? null;
  useEffect(() => (open && overlays !== null ? overlays.hold() : undefined), [open, overlays]);
}

/** Whether any of the room's dialogs is open. */
export function useRoomOverlayOpen(): boolean {
  const { overlays } = useRoom();
  return useSyncExternalStore(overlays.subscribe, () => overlays.count() > 0, () => false);
}

/** Where the room portals its dialogs: the host's container, or the body. */
export function useRoomPortal(): HTMLElement | undefined {
  return useContext(RoomContext)?.portalContainer ?? undefined;
}

/** The session whose document is being read here. Reads that name no session
 * of their own — a stored text member, an attachment's bytes — ride its client. */
const ReadingSessionContext = createContext<string | null>(null);
export const ReadingSessionProvider = ReadingSessionContext.Provider;

export function useReadingSession(): string | null {
  return useContext(ReadingSessionContext);
}
