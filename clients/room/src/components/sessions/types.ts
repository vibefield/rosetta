import type { Spend } from "../../lib/spend";

export interface SessionFacts {
  readonly tokens: number;
  readonly messages: number;
  /** List-price spend across the session's subagent tree. */
  readonly spend: Spend;
}
