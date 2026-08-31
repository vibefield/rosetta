// Which agent a `Co-authored-by` trailer names.
//
// A trailer is free text someone's tool wrote, so this is recognition, not
// lookup: it matches the addresses and names the coding agents actually sign
// with, and answers `null` for anything it does not know. A wrong mark is
// worse than no mark — it would attribute a person's commit to a machine —
// so every rule here is a match on something an agent puts there on purpose,
// never a guess from a name that merely looks automated.

import type { CoAuthor } from "./types";

interface Rule {
  /** rosetta runtime kind, which is what `AgentMark` draws. */
  readonly kind: string;
  /** Matched against the trailer's email, lowercased. */
  readonly email?: RegExp;
  /** Matched against the trailer's name, lowercased. */
  readonly name?: RegExp;
}

/** Ordered: the first match wins, so the specific addresses come before the
 * looser name patterns. */
const RULES: readonly Rule[] = [
  { kind: "claude-code", email: /@anthropic\.com$/ },
  { kind: "claude-code", name: /\bclaude\b/ },
  { kind: "codex", email: /^codex@|chatgpt@/ },
  { kind: "codex", name: /\bcodex\b/ },
  { kind: "cursor", email: /@cursor\.(com|sh)$/, },
  { kind: "cursor", name: /\bcursor\b/ },
  { kind: "github-copilot", email: /copilot@/ },
  { kind: "github-copilot", name: /\bcopilot\b/ },
  { kind: "gemini-cli", name: /\bgemini\b/ },
  { kind: "opencode", name: /\bopencode\b/ },
  { kind: "amp", email: /@ampcode\.com$/ },
  { kind: "windsurf", name: /\bwindsurf\b/ },
  { kind: "cline", name: /\bcline\b/ },
  { kind: "grok", name: /\bgrok\b/ },
  { kind: "kimi-code", name: /\bkimi\b/ },
  { kind: "antigravity", name: /\bantigravity\b/ },
  { kind: "devin", email: /@devin\.ai$/ },
];

/** The runtime kind this co-author is, or `null` when nothing recognises it —
 * which is how a human co-author stays a human. */
export function agentKindOf(author: CoAuthor): string | null {
  const email = (author.email ?? "").toLowerCase();
  const name = author.name.toLowerCase();
  for (const rule of RULES) {
    if (rule.email !== undefined && email !== "" && rule.email.test(email)) return rule.kind;
    if (rule.name !== undefined && rule.name.test(name)) return rule.kind;
  }
  return null;
}

export interface CreditedAgent {
  readonly kind: string;
  /** The trailer's own name — `Claude Opus 5 (1M context)` is more than the
   * kind knows, and it is what the reader recognises. */
  readonly label: string;
}

/** The agents credited on a commit, deduplicated by kind and in trailer
 * order. Two trailers for one agent is one mark, because the row is saying
 * WHICH agent, not how many times it signed. */
export function creditedAgents(coAuthors: readonly CoAuthor[]): readonly CreditedAgent[] {
  const seen = new Map<string, CreditedAgent>();
  for (const author of coAuthors) {
    const kind = agentKindOf(author);
    if (kind === null || seen.has(kind)) continue;
    seen.set(kind, { kind, label: author.name });
  }
  return [...seen.values()];
}
