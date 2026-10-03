// Solid marks beside monoline glyphs is deliberate: a brand mark is an
// identity and a category glyph is a type, and they should not read as one
// vocabulary. Ordered by share, opacity-ranked against the row's lead so a
// minor contributor never reads as disabled.
import { AGENT_PATHS } from "./agent-paths";

/** rosetta runtime kinds → bundled brand marks. */
const KIND_TO_MARK: Record<string, string> = {
  antigravity: "antigravity",
  "claude-code": "claudecode",
  codex: "codex",
  grok: "grok",
  hermes: "hermesagent",
  "kimi-code": "kimi",
  opencode: "opencode",
  pi: "pi",
  cursor: "cursor",
  "gemini-cli": "geminicli",
  "github-copilot": "githubcopilot",
  copilot: "githubcopilot",
  windsurf: "windsurf",
  amp: "amp",
  cline: "cline",
};

export function markIdForKind(kind: string): string | null {
  return KIND_TO_MARK[kind] ?? null;
}

export function agentLabel(kind: string): string {
  const id = markIdForKind(kind);
  if (id) return AGENT_PATHS[id]!.label;
  return (
    kind
      .split(/[-_]/)
      .filter(Boolean)
      .map((word) => word[0]!.toUpperCase() + word.slice(1))
      .join(" ")
  );
}

function initials(kind: string): string {
  const words = agentLabel(kind).split(/\s+/).filter(Boolean);
  return (words.length > 1 ? `${words[0]![0]}${words.at(-1)?.[0] ?? ""}` : words[0]?.slice(0, 2)!)
    .toUpperCase();
}

export function AgentMark({
  kind,
  opacity = 1,
  note,
}: {
  kind: string;
  opacity?: number;
  note?: string;
}) {
  const id = markIdForKind(kind);
  const label = agentLabel(kind);
  const name = note ? `${label} · ${note}` : label;
  if (!id) {
    return (
      <span
        className="ah-agent ah-agent--generic"
        role="img"
        style={{ opacity }}
        aria-label={name}
        title={name}
      >
        {initials(kind)}
      </span>
    );
  }
  const def = AGENT_PATHS[id]!;
  return (
    <span className="ah-agent" role="img" style={{ opacity }} aria-label={name} title={name}>
      <svg
        viewBox="0 0 24 24"
        fill="currentColor"
        fillRule="evenodd"
        aria-hidden="true"
        dangerouslySetInnerHTML={{ __html: def.path }}
      />
    </span>
  );
}

export interface AgentShare {
  kind: string;
  pct: number;
}

export function AgentBadges({ shares, cap = 3 }: { shares: readonly AgentShare[]; cap?: number }) {
  if (shares.length === 0) return <span className="ah-agents" />;
  const shown = shares.slice(0, cap);
  const extra = shares.length - shown.length;
  const lead = shown[0]!.pct || 1;
  return (
    <span className="ah-agents">
      {shown.map((s) => (
        <AgentMark
          key={s.kind}
          kind={s.kind}
          opacity={0.34 + 0.3 * (s.pct / lead)}
          note={`${s.pct}% of this project's sessions`}
        />
      ))}
      {extra > 0 ? <span className="ah-more">+{extra}</span> : null}
    </span>
  );
}
