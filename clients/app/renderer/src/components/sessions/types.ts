export interface SessionFacts {
  readonly tokens: number;
  readonly messages: number;
}

export type SessionScope = "roots" | "all";
