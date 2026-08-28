// Solid marks beside monoline glyphs is deliberate: a brand mark is an
// identity and a category glyph is a type, and they should not read as one
// vocabulary. Ordered by share, opacity-ranked against the row's lead so a
// minor contributor never reads as disabled.
import { AGENT_PATHS } from "./agent-paths";

/** rosetta runtime kinds → lobehub mark ids. Unknown kinds get no mark. */
const KIND_TO_MARK: Record<string, string> = {
  "claude-code": "claudecode",
  codex: "codex",
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
  return id ? AGENT_PATHS[id].label : kind;
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
  if (!id) return null;
  const def = AGENT_PATHS[id];
  const name = note ? `${def.label} · ${note}` : def.label;
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

export function AgentBadges({ shares, cap = 3 }: { shares: AgentShare[]; cap?: number }) {
  const marked = shares.filter((s) => markIdForKind(s.kind) !== null);
  if (marked.length === 0) return <span className="ah-agents" />;
  const shown = marked.slice(0, cap);
  const extra = marked.length - shown.length;
  const lead = shown[0].pct || 1;
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
