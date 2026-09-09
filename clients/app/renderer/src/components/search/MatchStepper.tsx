// The stepper — what the sheet leaves behind when a hit lands. The head of
// the transcript says what the reader is doing (`debounce · 3 of 7`) and lets
// them walk the session's other matches without reopening the sheet, the way
// an editor's find bar walks a file. ⌘G / ⇧⌘G are the platform's own
// find-next / find-previous; the arrows are the same acts with a pointer.
import { CloseGlyph } from "../../lib/icons";
import { shortcutLabel } from "./shortcut";

export interface MatchStepperProps {
  readonly query: string;
  /** Zero-based position of the current match. */
  readonly at: number;
  readonly total: number;
  readonly onStep: (delta: 1 | -1) => void;
  readonly onDismiss: () => void;
}

function Chevron({ direction }: { readonly direction: "up" | "down" }) {
  return (
    <svg viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path
        d={direction === "up" ? "M4 10l4-4 4 4" : "M4 6l4 4 4-4"}
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function MatchStepper({ query, at, total, onStep, onDismiss }: MatchStepperProps) {
  return (
    <div
      className="ah-stepper"
      role="group"
      aria-label={`${total} ${total === 1 ? "match" : "matches"} for ${query} in this session`}
    >
      <b title={query}>{query}</b>
      <span className="tnum" aria-live="polite">
        {at + 1} of {total}
      </span>
      <button
        type="button"
        aria-label={`Previous match (${shortcutLabel("G", true)})`}
        title={`Previous match · ${shortcutLabel("G", true)}`}
        disabled={total <= 1}
        onClick={() => onStep(-1)}
      >
        <Chevron direction="up" />
      </button>
      <button
        type="button"
        aria-label={`Next match (${shortcutLabel("G")})`}
        title={`Next match · ${shortcutLabel("G")}`}
        disabled={total <= 1}
        onClick={() => onStep(1)}
      >
        <Chevron direction="down" />
      </button>
      <button type="button" aria-label="Stop stepping through matches" title="Done" onClick={onDismiss}>
        <CloseGlyph />
      </button>
    </div>
  );
}
