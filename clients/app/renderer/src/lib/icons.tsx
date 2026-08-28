// The room's own glyphs, drawn on mille's 16-unit grid at stroke 1.5 (or the
// strip's 24-grid where the proto drew them there) — transcribed from
// protos/agent-history-proto.

export function SearchGlyph() {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.9"
      strokeLinecap="round"
      aria-hidden="true"
    >
      <circle cx="10.8" cy="10.8" r="6.3" />
      <path d="m15.4 15.4 4.1 4.1" />
    </svg>
  );
}

/** The git glyph — one shape for git everywhere in this room. */
export function GitGlyph() {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M5 5.6v4.9" />
      <path d="M11 7.9v.7a3.2 3.2 0 0 1-3.2 3.2H6.4" />
      <circle cx="5" cy="4" r="1.6" fill="currentColor" stroke="none" />
      <circle cx="5" cy="12" r="1.6" fill="currentColor" stroke="none" />
      <circle cx="11" cy="6.3" r="1.6" fill="currentColor" stroke="none" />
    </svg>
  );
}

/** Three ascending bars — the figures toggle. */
export function StatsGlyph() {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      aria-hidden="true"
    >
      <path d="M3.7 12.1V9.3" />
      <path d="M8 12.1V6.5" />
      <path d="M12.3 12.1V4.1" />
    </svg>
  );
}

export function ChevronGlyph() {
  return (
    <svg viewBox="0 0 12 12" fill="none" aria-hidden="true">
      <path
        d="M4.5 2.5 8 6 4.5 9.5"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
