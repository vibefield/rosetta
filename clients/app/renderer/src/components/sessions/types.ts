import type { SessionRow } from "@rosetta/protocol";
import type { ProjectViewModel } from "../../lib/model";

export interface SessionFacts {
  readonly tokens: number;
  readonly messages: number;
}

export interface SessionSearchResult {
  readonly session: SessionRow;
  readonly hitCount: number;
}

export interface SessionSearchGroup {
  readonly project: ProjectViewModel;
  readonly sessions: readonly SessionSearchResult[];
}

export type SessionScope = "roots" | "all";
