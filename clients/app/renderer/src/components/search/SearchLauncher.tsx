// The launcher — the door in the strip. It looks like the field it replaced
// (same pill, same glyph, same words) and sits where DESIGN.md §10 seats the
// file pill: centred in the drag strip, clear of the traffic lights. It does
// not take text; pressing it grows it into the sheet. When a search is
// active it says so in primary ink, because a filtered room should never
// look like an unfiltered one (design record §09: the scope of a search is
// not the reader's to guess).
import { SearchGlyph } from "../../lib/icons";
import { shortcutLabel } from "./shortcut";

export interface SearchLauncherProps {
  /** The remembered query, or "" when nothing is being searched. */
  readonly query: string;
  /** The honest scope, e.g. `Search 6,720 sessions across 9 agents`. */
  readonly placeholder: string;
  readonly onOpen: () => void;
  readonly onClear: () => void;
}

export function SearchLauncher({ query, placeholder, onOpen, onClear }: SearchLauncherProps) {
  const active = query !== "";
  return (
    <div className="ah-launcher" data-active={active || undefined}>
      <button
        type="button"
        className="ah-launcher__open"
        aria-haspopup="dialog"
        aria-label={active ? `Searching for ${query} — press to change` : placeholder}
        title={active ? `Searching for ${query}` : undefined}
        onClick={onOpen}
      >
        <SearchGlyph />
        <span className="ah-launcher__text">{active ? query : placeholder}</span>
        <kbd className="ah-kbd" aria-hidden="true">
          {shortcutLabel("K")}
        </kbd>
      </button>
      {active && (
        <button
          type="button"
          className="ah-launcher__clear"
          aria-label="Clear the search"
          onClick={onClear}
        >
          ×
        </button>
      )}
    </div>
  );
}
