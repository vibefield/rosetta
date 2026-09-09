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
      // This is ingestion completeness, never evidence of an active agent.
      return {
        tone: "orange",
        isLive: false,
        label: duringBackfill ? "indexing" : "history incomplete",
      };
    case "pending":
      return { tone: "orange", isLive: false, label: "not yet indexed" };
    default:
      return { tone: null, isLive: false, label: "history indexed" };
  }
}

export function getSessionTitle(session: SessionRow): string {
  return session.title ?? session.summary ?? session.native_session_id;
}
