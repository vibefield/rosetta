// The room-level search strip: a line across the head of the room, never a
// modal. The scope sentence names the vendors actually indexed (§9 — the
// scope of a search is not the user's to guess).
import { SearchGlyph } from "../lib/icons";

export function SearchStrip({
  query,
  onQuery,
  scope,
}: {
  query: string;
  onQuery: (q: string) => void;
  scope: string;
}) {
  return (
    <div className="ah-searchbar">
      <div className="ah-search">
        <SearchGlyph />
        <input
          type="search"
          value={query}
          placeholder="Search your whole agentic past"
          aria-label="Search your whole agentic past"
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => onQuery(e.target.value)}
        />
        {query !== "" && (
          <button type="button" aria-label="Clear search" onClick={() => onQuery("")}>
            ×
          </button>
        )}
      </div>
      <p className="ah-scope">{scope}</p>
    </div>
  );
}
