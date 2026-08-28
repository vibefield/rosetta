import type { SessionRow } from "@rosetta/protocol";

export interface SessionStatus {
  readonly tone: "green" | "orange" | null;
  readonly isLive: boolean;
  readonly label: string;
}

export function getSessionStatus(session: SessionRow): SessionStatus {
  switch (session.stream_state) {
    case "streaming":
      return { tone: "green", isLive: true, label: "live" };
    case "pending":
      return { tone: "orange", isLive: false, label: "not yet indexed" };
    default:
      return { tone: null, isLive: false, label: "done" };
  }
}

export function getSessionTitle(session: SessionRow): string {
  return session.title ?? session.summary ?? session.native_session_id;
}
