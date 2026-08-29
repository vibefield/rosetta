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

/** Root and two branches — the worktree toggle, on mille's 16-unit grid. */
export function TreeGlyph() {
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
      <path d="M4.3 4.6v6.9" />
      <path d="M4.3 7.6h3.3M4.3 11.5h3.3" />
      <circle cx="4.3" cy="3.2" r="1.5" fill="currentColor" stroke="none" />
      <circle cx="9.3" cy="7.6" r="1.5" fill="currentColor" stroke="none" />
      <circle cx="9.3" cy="11.5" r="1.5" fill="currentColor" stroke="none" />
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

/** Copy — two sheets, the front one offset. */
export function CopyGlyph() {
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
      <rect x="5.5" y="5.5" width="8" height="8" rx="2" />
      <path d="M10.5 3.5v-.5a1.5 1.5 0 0 0-1.5-1.5H4A1.5 1.5 0 0 0 2.5 3v5A1.5 1.5 0 0 0 4 9.5h.5" />
    </svg>
  );
}

/** Leaving the app — an arrow out of a frame that is deliberately open on the
 * side it leaves by. */
export function ExternalGlyph() {
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
      <path d="M13 9.5V13a1.5 1.5 0 0 1-1.5 1.5H3A1.5 1.5 0 0 1 1.5 13V4.5A1.5 1.5 0 0 1 3 3h3.5" />
      <path d="M9.5 1.5h5v5" />
      <path d="m14.5 1.5-6.5 6.5" />
    </svg>
  );
}

export function CloseGlyph() {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      aria-hidden="true"
    >
      <path d="m4.5 4.5 7 7M11.5 4.5l-7 7" />
    </svg>
  );
}
