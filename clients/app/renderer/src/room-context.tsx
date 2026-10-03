// The ports, as the room's parts read them. `HistoryRoom` provides them; a
// harness that mounts one column on its own wraps it in `RoomProvider`.
import { createContext, useContext, useMemo } from "react";
import type { ReactNode } from "react";
import type { AssetSource, CatalogPort, HostPort, RoomPorts } from "./ports";
import { createSessionLeases } from "./lib/session-leases";
import type { SessionLeases } from "./lib/session-leases";

export interface Room {
  readonly catalog: CatalogPort;
  readonly sessions: SessionLeases;
  readonly assets: AssetSource;
  readonly host: HostPort | null;
}

const RoomContext = createContext<Room | null>(null);

export function RoomProvider({ ports, children }: { readonly ports: RoomPorts; readonly children?: ReactNode }) {
  const { catalog, sessions: sessionPort, assets } = ports;
  const host = ports.host ?? null;
  const sessions = useMemo(() => createSessionLeases(sessionPort), [sessionPort]);
  const room = useMemo<Room>(
    () => ({ catalog, sessions, assets, host }),
    [catalog, sessions, assets, host],
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

/** The session whose document is being read here. Reads that name no session
 * of their own — a stored text member, an attachment's bytes — ride its client. */
const ReadingSessionContext = createContext<string | null>(null);
export const ReadingSessionProvider = ReadingSessionContext.Provider;

export function useReadingSession(): string | null {
  return useContext(ReadingSessionContext);
}
