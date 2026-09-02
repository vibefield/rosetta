// The ledger's glyph set — §04's monoline drawings, one per FAMILY.
//
// The record's rule, kept: 16-unit grid, stroke 1.5, `currentColor`, no fill.
// A file's type is not a state and neither is a tool's, so `Created` and
// `Edited` share the delta glyph and the VERB carries the operation. What
// substitutes the glyph is STATE — a failed line draws the cross, a running
// line in a live session draws the breathing dot — which is why those two are
// here beside the families rather than in the table.
//
// They live in this folder rather than `lib/icons.tsx` because they are the
// transcript's vocabulary, not the room's: the room's glyphs name places to
// go, and these name things that were done.
import type { ReactNode } from "react";
import type { LedgerGlyph } from "../../lib/transcript";

const ROUND = { strokeLinecap: "round", strokeLinejoin: "round" } as const;

/** Every family glyph, on the record's own paths. */
const PATHS: Readonly<Record<LedgerGlyph, ReactNode>> = {
  read: (
    <>
      <path d="M4 2.5h5.5L13 6v7.5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-10a1 1 0 0 1 1-1z" />
      <path d="M9.5 2.5V6H13" />
    </>
  ),
  list: <path d="M3 4.5h10M3 8h10M3 11.5h6" />,
  search: (
    <>
      <circle cx="7" cy="7" r="4.2" />
      <path d="m10.2 10.2 3.3 3.3" />
    </>
  ),
  web: (
    <>
      <circle cx="8" cy="8" r="5.5" />
      <path d="M2.5 8h11M8 2.5c2.2 2.2 2.2 8.8 0 11M8 2.5c-2.2 2.2-2.2 8.8 0 11" />
    </>
  ),
  shell: (
    <>
      <path d="m3 4.5 3.5 3.5L3 11.5" />
      <path d="M8.5 12h4.5" />
    </>
  ),
  delta: (
    <>
      <path d="M5.5 2.5v5M3 5h5" />
      <path d="M8 12h5" />
    </>
  ),
  delegate: (
    <>
      <path d="M4 2.5v6.5a2 2 0 0 0 2 2h7" />
      <path d="m10.5 8.5 2.5 2.5-2.5 2.5" />
    </>
  ),
  task: (
    <>
      <rect x="3" y="3" width="10" height="10" rx="2" />
      <path d="m5.5 8 2 2 3.5-4" />
    </>
  ),
  question: (
    <>
      <circle cx="8" cy="8" r="5.5" />
      <path d="M6.3 6.6a1.7 1.7 0 1 1 2.6 1.4c-.6.4-.9.7-.9 1.3" />
      <path d="M8 11.3v.1" />
    </>
  ),
  doc: (
    <>
      <rect x="3" y="2.5" width="10" height="11" rx="1.5" />
      <path d="M5.5 6h5M5.5 8.5h5M5.5 11h3" />
    </>
  ),
  bell: (
    <>
      <path d="M5 11V7.5a3 3 0 0 1 6 0V11l1 1H4z" />
      <path d="M6.8 13.5a1.3 1.3 0 0 0 2.4 0" />
    </>
  ),
  box: (
    <>
      <path d="M2.5 5 8 2.5 13.5 5 8 7.5z" />
      <path d="M2.5 5v6L8 13.5l5.5-2.5V5M8 7.5v6" />
    </>
  ),
  plug: (
    <>
      <path d="M6 2.5v3M10 2.5v3M4 5.5h8v2a4 4 0 0 1-8 0z" />
      <path d="M8 11.5v2" />
    </>
  ),
  flag: <path d="M4 13.5V3h7l-1.5 2.5L11 8H4" />,
  bolt: <path d="M9 2 4 9h4l-1 5 5-7H8z" />,
  sliders: (
    <>
      <path d="M3 5h3M9 5h4M3 11h6M12 11h1" />
      <circle cx="7.5" cy="5" r="1.5" />
      <circle cx="10.5" cy="11" r="1.5" />
    </>
  ),
  clock: (
    <>
      <circle cx="8" cy="8" r="5.5" />
      <path d="M8 5v3.2l2 1.3" />
    </>
  ),
  help: (
    <>
      <path d="M6.3 6.6a1.7 1.7 0 1 1 2.6 1.4c-.6.4-.9.7-.9 1.3" />
      <path d="M8 11.3v.1" />
    </>
  ),
};

export function FamilyGlyph({ glyph }: { readonly glyph: LedgerGlyph }) {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" {...ROUND} aria-hidden="true">
      {PATHS[glyph]}
    </svg>
  );
}

/** T-5's one hue: the act did not complete. Substitutes the family glyph. */
export function FailGlyph() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" aria-hidden="true">
      <path d="m4.5 4.5 7 7M11.5 4.5l-7 7" />
    </svg>
  );
}

/** The disclosure, on the 12-unit grid the record drew it on. */
export function LineChevron() {
  return (
    <svg viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" {...ROUND} aria-hidden="true">
      <path d="M4.5 2.5 8 6 4.5 9.5" />
    </svg>
  );
}

/** The column head's activity act — fold or open every run at once (§05). */
export function ActivityGlyph() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" {...ROUND} aria-hidden="true">
      <path d="M2.5 8h2.5l2-4.5 2 9 2-4.5h2.5" />
    </svg>
  );
}
