// A web line's site icon, and the stack of them at a line's edge.
//
// The design record's rule, in one sentence: the icon is content, so it keeps
// its colour — and it lives in the source list and in a small stack at the
// line's right, never in the glyph slot, which stays monoline. A site whose
// icon was not fetched wears the globe.
//
// Everything upstream of drawing lives in `lib/transcript/favicons.ts`: the
// setting, the shared per-host map, and the ask that the Electron shell
// answers. This file only decides what a result LOOKS like.

import { useState } from "react";
import type { CSSProperties } from "react";
import { useFavicon } from "../../lib/transcript/favicons";
import "../../styles/favicon.css";

/** The globe, on this room's 16-unit grid at stroke 1.5 — the same drawing
 * the design record used for a web line, which is deliberate: the fallback
 * for "no icon" is the glyph the line already wears.
 *
 * It lives here rather than in `lib/icons.tsx` only because that file is
 * being edited elsewhere this session; it belongs there. */
function GlobeGlyph() {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      aria-hidden="true"
    >
      <circle cx="8" cy="8" r="5.5" />
      <path d="M2.5 8h11M8 2.5c2.2 2.2 2.2 8.8 0 11M8 2.5c-2.2 2.2-2.2 8.8 0 11" />
    </svg>
  );
}

export interface FaviconProps {
  /** The site, e.g. `developer.mozilla.org`. Null draws the globe, which is
   * what a source with no parseable url honestly is. */
  readonly host: string | null;
  /** Pixels. 14 is the record's size and the only one the room uses today. */
  readonly size?: number;
}

/**
 * One site's icon: its own, in colour, on a hairline ring — or the globe.
 *
 * The globe covers four different facts that are one fact to a reader: the
 * setting is off, the shell refused the host, the site has no icon, or this
 * is a browser tab with no shell to ask. None of them is an error worth a
 * reader's attention, so none of them says so.
 */
export function Favicon({ host, size = 14 }: FaviconProps) {
  const { src, state } = useFavicon(host);
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  // The host is the label because it is the fact the icon stands for; the
  // title beside it in a source list is the same information twice.
  const label = host ?? "unknown site";
  // A custom property, so one rule sizes the plate, the ring and the image
  // together; the default lives in the stylesheet rather than here.
  const style =
    size === 14 ? undefined : ({ "--tr-fav-size": `${size}px` } as CSSProperties);

  if (state === "ready" && src !== null && src !== failedSrc) {
    return (
      <span className="tr-fav" style={style} title={label}>
        {/* `alt` is empty and the span carries the title: the icon adds no
            information a screen reader has not already been given by the
            host beside it. An `alt` of "github.com" would read it twice. */}
        <img
          src={src}
          alt=""
          loading="lazy"
          decoding="async"
          draggable={false}
          onError={() => setFailedSrc(src)}
        />
      </span>
    );
  }

  if (state === "loading") {
    return <span className="tr-fav tr-fav--loading" style={style} aria-hidden="true" />;
  }

  return (
    <span className="tr-fav tr-fav--globe" style={style} title={label}>
      <GlobeGlyph />
    </span>
  );
}

export interface FaviconStackProps {
  /** The sources' hosts, in the order the search returned them. */
  readonly hosts: readonly (string | null)[];
  /** How many to draw. Three is the record's stack. */
  readonly max?: number;
}

/**
 * The first few sites of a search, overlapping, at a line's right edge.
 *
 * It is a summary, not a list: the count beside it is what says how many
 * there were, and the stack says what kind of places they were. Duplicate
 * hosts collapse — five MDN pages are one site, and five identical icons
 * would claim otherwise.
 */
export function FaviconStack({ hosts, max = 3 }: FaviconStackProps) {
  if (max <= 0) return null;
  const seen: (string | null)[] = [];
  for (const host of hosts) {
    if (host !== null && seen.includes(host)) continue;
    seen.push(host);
    if (seen.length === max) break;
  }
  if (seen.length === 0) return null;
  return (
    <span className="tr-favs">
      {seen.map((host, index) => (
        <Favicon key={host ?? `unknown-${index}`} host={host} />
      ))}
    </span>
  );
}
