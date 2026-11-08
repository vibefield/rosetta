// The hold — a wait that fills a place, in the host's mark (R10,
// `RoomWaitMark`). The mark at the centre of its place, one line 26 under it
// and its count 4 under that, so a host's own load and the room's read as one
// look (VibeField holds its Setup Assistant's loads so). Late, as the veil is:
// nothing for 120ms, then the words over 180ms and the mark over 400ms — at
// once when the host's own hold already stands there (`carried`). A new line
// crossfades in place, 140ms out and then 240ms in; a new count under the same
// line only counts. The mark only ever fades: it never moves or scales, and it
// stands at rest while its work is paused. Drawn only while the host gives a
// mark: without one the room's own waits stand, as rosetta's app shows them.
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useHostInputs } from "../../host-inputs";
import type { RoomWaitPlace } from "../../host-inputs";
import "../../styles/hold.css";

/** The words' leaving, before a new line comes in. */
const LINE_OUT_MS = 140;

/** A hold's words: its line, and the count under it. */
export interface HoldWords {
  /** What makes a new line, and so its crossfade: the words' identity, never
   * their render (a count under the same line only counts). */
  readonly key: string;
  readonly line: ReactNode;
  readonly sub?: ReactNode;
}

type LinePhase = "first" | "out" | "in";

/** The line as it shows: the old words while they leave, then the new. Words
 * that come back while their line is leaving come in again at once. */
function useCrossfade(words: HoldWords): { readonly words: HoldWords; readonly phase: LinePhase } {
  const [shown, setShown] = useState<{ words: HoldWords; phase: LinePhase }>({ words, phase: "first" });
  const latest = useRef(words);
  latest.current = words;
  const changed = words.key !== shown.words.key;
  const leaving = shown.phase === "out";
  useEffect(() => {
    if (!changed) {
      if (leaving) setShown({ words: latest.current, phase: "in" });
      return undefined;
    }
    if (!leaving) {
      setShown((now) => ({ words: now.words, phase: "out" }));
      return undefined;
    }
    const timer = setTimeout(() => setShown({ words: latest.current, phase: "in" }), LINE_OUT_MS);
    return () => clearTimeout(timer);
  }, [changed, leaving, words.key]);
  return changed ? shown : { words, phase: shown.phase };
}

export function Hold({ place, words, resting = false, carried = false, leaving = false, face, act }: {
  readonly place: Exclude<RoomWaitPlace, "card">;
  readonly words: HoldWords;
  /** Its work is paused: the mark stands at rest. */
  readonly resting?: boolean;
  /** The host's own hold stood here as this one came: at once, no fade. */
  readonly carried?: boolean;
  /** Its wait is over and it fades away (280ms) before what comes next. */
  readonly leaving?: boolean;
  /** The face it stands for, as the room's other faces name theirs. */
  readonly face: string;
  /** An act under its words, where the wait offers one. */
  readonly act?: ReactNode;
}) {
  const { waitMark } = useHostInputs();
  const shown = useCrossfade(words);
  return (
    <div className="ah-hold" data-place={place} data-face={face}
      data-carried={carried ? "" : undefined} data-leaving={leaving ? "" : undefined}
      role="status" aria-live="polite" aria-atomic="true">
      <div className="ah-hold__mark" aria-hidden="true">{waitMark?.mark(place, { resting })}</div>
      <div className="ah-hold__words" data-line={shown.phase}>
        <p className="ah-hold__line">{shown.words.line}</p>
        <p className="ah-hold__sub">{shown.words.sub}</p>
        {act !== undefined && act !== null && <div className="ah-hold__act">{act}</div>}
      </div>
    </div>
  );
}
