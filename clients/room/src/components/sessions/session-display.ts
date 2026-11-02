import type { SessionRow } from "@vibefield/rosetta-protocol";

export interface SessionStatus {
  readonly tone: "green" | "orange" | null;
  readonly isLive: boolean;
  readonly label: string;
}

export function getSessionStatus(
  session: SessionRow,
  duringBackfill = false,
  /** A row the metadata pass made only for its children: no import will
   * reach it, so nothing about it is cooking (`isMetadataOnly`). */
  metadataOnly = false,
): SessionStatus {
  if (metadataOnly) return { tone: null, isLive: false, label: "transcript not available" };
  switch (session.stream_state) {
    case "streaming":
      // This is ingestion completeness, never evidence of an active agent.
      return {
        tone: "orange",
        isLive: false,
        label: duringBackfill ? "indexing" : "history incomplete",
      };
    case "pending":
      // Newest trees import first: while the import runs, an older one
      // is simply not reached yet.
      return { tone: "orange", isLive: false, label: duringBackfill ? "not imported yet" : "not yet indexed" };
    default:
      return { tone: null, isLive: false, label: "history indexed" };
  }
}

export function getSessionTitle(session: SessionRow): string {
  return session.title ?? session.summary ?? session.native_session_id;
}
