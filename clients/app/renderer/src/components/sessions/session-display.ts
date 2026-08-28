import type { SessionRow } from "@rosetta/protocol";

export interface SessionStatus {
  readonly tone: "green" | "orange" | null;
  readonly isLive: boolean;
  readonly label: string;
}

export function getSessionStatus(
  session: SessionRow,
  duringBackfill = false,
): SessionStatus {
  switch (session.stream_state) {
    case "streaming":
      // The same green breath means two honest things: mid-backfill the
      // daemon is reading this transcript right now; live, the agent is.
      return {
        tone: "green",
        isLive: true,
        label: duringBackfill ? "indexing" : "live",
      };
    case "pending":
      return { tone: "orange", isLive: false, label: "not yet indexed" };
    default:
      return { tone: null, isLive: false, label: "done" };
  }
}

export function getSessionTitle(session: SessionRow): string {
  return session.title ?? session.summary ?? session.native_session_id;
}
