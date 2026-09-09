// The search sheet — the door in the strip, grown into the tool it opens.
//
// A top-attached sheet (DESIGN.md §8's docs explorer, one level wider): the
// field where the launcher was, a scope row that says what is being searched,
// a list the keyboard walks, and a foot that tells the truth with numbers.
// Radix owns what a dialog must not get wrong — the portal, the focus trap,
// Escape, the outside press — and the input owns the cursor, APG combobox
// style: focus never leaves the field; `aria-activedescendant` names the row.
//
// What lands where is `lib/search-sheet.ts`; this file only draws it.
import * as Dialog from "@radix-ui/react-dialog";
import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import type {
  SearchBuildProgress,
  SearchHit,
  SearchReadiness,
  SessionRow,
} from "@rosetta/protocol";
import { AgentMark, agentLabel } from "../../lib/agents";
import { dayLabel } from "../../lib/format";
import { sessionLastActive } from "../../lib/session-activity";
import { SEARCH_LIMIT } from "../../lib/hooks";
import type { SearchFeed } from "../../lib/hooks";
import { SearchGlyph } from "../../lib/icons";
import { parseSnippet } from "../../lib/model";
import type { ProjectViewModel } from "../../lib/model";
import type { SearchResultsViewModel } from "../../lib/search-results";
import {
  KIND_FILTER_OPTIONS,
  buildSheetLines,
  cleanSnippet,
  forgetQuery,
  hitWhen,
  isCursorKey,
  kindWord,
  moveCursor,
  moveSection,
  readRecentQueries,
  sessionCrumb,
  stateSentence,
} from "../../lib/search-sheet";
import type {
  RecentStore,
  SearchFilters,
  SearchKindFilter,
  SearchScopeFilter,
  SheetHead,
  SheetOption,
} from "../../lib/search-sheet";
import { ActivityGlyph } from "../transcript/glyphs";
import { SegmentedControl } from "../ui";
import { hasPrimaryModifier } from "./shortcut";

const LIST_ID = "ah-sheet-listbox";

export interface SearchSheetProps {
  readonly open: boolean;
  readonly onClose: () => void;
  readonly query: string;
  readonly onQuery: (query: string) => void;
  readonly filters: SearchFilters;
  readonly onFilters: (filters: SearchFilters) => void;
  readonly feed: SearchFeed;
  readonly results: SearchResultsViewModel | null;
  readonly readiness: SearchReadiness | null;
  readonly progress: SearchBuildProgress | null;
  readonly canonicalReady: boolean | null;
  readonly ingestPaused: boolean;
  /** Runtime families the daemon reads, for the agent chip and the scope line. */
  readonly agentKinds: readonly string[];
  /** The selected project, so the sheet can offer to stay inside it. */
  readonly currentProject: ProjectViewModel | null;
  readonly homeDirectory: string | null;
  readonly nowMs: number;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly onOpenHit: (session: SessionRow, hit: SearchHit) => void;
}

function safeStorage(): RecentStore | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

type TintStyle = CSSProperties & { "--tint": string };

/** Who said it, drawn the way the transcript draws them: the prompt is the
 * one filled shape, a reply wears the agent's mark, activity the ledger's
 * glyph, and everything else a quiet dot. */
function WhoGlyph({ kind, runtimeKind }: { readonly kind: string; readonly runtimeKind: string }) {
  switch (kind) {
    case "user":
      return <i className="ah-sheet__you" />;
    case "agent":
      return <AgentMark kind={runtimeKind} />;
    case "tool_use":
      return <ActivityGlyph />;
    case "thinking":
      return (
        <svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
          <circle cx="3.5" cy="8" r="1.4" />
          <circle cx="8" cy="8" r="1.4" />
          <circle cx="12.5" cy="8" r="1.4" />
        </svg>
      );
    default:
      return <i className="ah-sheet__dot" />;
  }
}

export function headDomId(sessionId: string): string {
  return `ah-sheet-head-${sessionId}`;
}

function HeadLine({
  head,
  runtimeKind,
  nowMs,
}: {
  readonly head: SheetHead;
  readonly runtimeKind: string;
  readonly nowMs: number;
}) {
  const crumb = sessionCrumb(head.session);
  return (
    <div
      id={headDomId(head.session.id)}
      className="ah-sheet__head"
      role="presentation"
      style={{ "--tint": head.project.tint } as TintStyle}
    >
      <i aria-hidden="true" />
      <span className="ah-sheet__crumb">
        <b>{head.project.row.name}</b>
        <i aria-hidden="true">›</i>
        <span
          data-mono={crumb.mono || undefined}
          title={crumb.mono ? `Native session ${head.session.native_session_id}` : crumb.text}
        >
          {crumb.text}
        </span>
      </span>
      <AgentMark kind={runtimeKind} />
      <span className="ah-sheet__headmeta">
        <span>
          {head.hitCount} {head.hitCount === 1 ? "hit" : "hits"}
        </span>
        <span aria-hidden="true">·</span>
        <span>{dayLabel(sessionLastActive(head.session), nowMs)}</span>
      </span>
    </div>
  );
}

function OptionRow({
  option,
  active,
  home,
  nowMs,
  runtimeKind,
  onHover,
  onOpen,
}: {
  readonly option: SheetOption;
  readonly active: boolean;
  readonly home: string | null;
  readonly nowMs: number;
  readonly runtimeKind: string;
  readonly onHover: () => void;
  readonly onOpen: () => void;
}) {
  const { hit, project, session } = option;
  const parts = parseSnippet(cleanSnippet(hit.snippet, home));
  const who = kindWord(hit.kind, agentLabel(runtimeKind));
  const when = hitWhen(hit.at, nowMs);
  const crumb = sessionCrumb(session);
  const spoken = parts.map((part) => part.text).join("");
  return (
    <div
      id={option.id}
      role="option"
      aria-selected={active}
      aria-label={`${who}: ${spoken} · ${when} · ${project.row.name} › ${crumb.text}`}
      className="ah-sheet__row"
      onPointerMove={onHover}
      onClick={onOpen}
    >
      <span className="ah-sheet__who" title={who} aria-hidden="true">
        <WhoGlyph kind={hit.kind} runtimeKind={runtimeKind} />
      </span>
      <p className="ah-sheet__snippet">
        {parts.map((part, at) =>
          part.mark ? <mark key={at}>{part.text}</mark> : <span key={at}>{part.text}</span>,
        )}
      </p>
      <span className="ah-sheet__when">{when}</span>
    </div>
  );
}

function Quiet({ title, children }: { readonly title: string; readonly children: ReactNode }) {
  return (
    <div className="ah-sheet__quiet" role="presentation">
      <strong>{title}</strong>
      <p>{children}</p>
    </div>
  );
}

const GRAMMAR_HINT = (
  <>
    Every word must match. Put a phrase in quotes to match it exactly — <code>"cargo test"</code>.
    Opening a result lands the transcript on that message.
  </>
);

export function SearchSheet({
  open,
  onClose,
  query,
  onQuery,
  filters,
  onFilters,
  feed,
  results,
  readiness,
  progress,
  canonicalReady,
  ingestPaused,
  agentKinds,
  currentProject,
  homeDirectory,
  nowMs,
  getRuntimeKind,
  onOpenHit,
}: SearchSheetProps) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const { lines, options } = useMemo(() => buildSheetLines(results), [results]);
  const emptyQuery = query.trim() === "";
  const [recent, setRecent] = useState<string[]>([]);
  const [active, setActive] = useState(-1);
  // A keyboard move scrolls the cursor into view; a pointer move must not,
  // or the list would crawl under a resting mouse.
  const movedBy = useRef<"keyboard" | "pointer">("keyboard");

  useEffect(() => {
    if (open) setRecent(readRecentQueries(safeStorage()));
  }, [open]);

  // New rows → the first one is the cursor, so Enter lands at once. Keyed on
  // the rows' identities, not the array: the model is rebuilt on every clock
  // tick, and a cursor that jumped home every thirty seconds would be a bug.
  const walking = emptyQuery ? recent.length : options.length;
  const rowsKey = emptyQuery ? `recent\n${recent.join("\n")}` : options.map((o) => o.id).join("\n");
  useEffect(() => {
    movedBy.current = "keyboard";
    setActive(rowsKey === "" || rowsKey === "recent\n" ? -1 : 0);
  }, [rowsKey]);

  const listRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (movedBy.current !== "keyboard" || active < 0) return;
    const id = emptyQuery ? `ah-sheet-recent-${active}` : options[active]?.id;
    const list = listRef.current;
    const row = id === undefined ? null : document.getElementById(id);
    if (list === null || row === null) return;
    // The list scrolls itself; `scrollIntoView` would also nudge the sheet.
    const frame = list.getBoundingClientRect();
    const rect = row.getBoundingClientRect();
    const top = rect.top - frame.top + list.scrollTop;
    const headroom = 30; // a pinned head sits over the first rows
    if (rect.top < frame.top + headroom) list.scrollTop = top - headroom;
    else if (rect.bottom > frame.bottom) list.scrollTop = top - (list.clientHeight - rect.height) + 4;
  }, [active, emptyQuery, options]);

  const hover = (index: number) => {
    if (index === active) return;
    movedBy.current = "pointer";
    setActive(index);
  };

  // A chip is pressed with the pointer and then forgotten: the field takes
  // focus back, so the next keystroke searches and Enter still opens a row.
  const narrow = (next: SearchFilters) => {
    onFilters(next);
    inputRef.current?.focus();
  };

  const onKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    // ⌘↑ / ⌘↓ jump a whole session, the way VS Code's sticky separators and
    // Raycast's sections let the keyboard past a long group.
    if (
      !emptyQuery &&
      hasPrimaryModifier(event) &&
      (event.key === "ArrowDown" || event.key === "ArrowUp")
    ) {
      if (options.length === 0) return;
      event.preventDefault();
      movedBy.current = "keyboard";
      const direction = event.key === "ArrowDown" ? 1 : -1;
      setActive((current) => moveSection(current, direction, options));
      return;
    }
    if (isCursorKey(event.key)) {
      if (walking === 0) return;
      event.preventDefault();
      movedBy.current = "keyboard";
      const key = event.key;
      setActive((current) => moveCursor(current, key, walking));
      return;
    }
    if (event.key !== "Enter" || event.nativeEvent.isComposing) return;
    event.preventDefault();
    if (emptyQuery) {
      const remembered = recent[active];
      if (remembered !== undefined) onQuery(remembered);
      return;
    }
    const option = options[active];
    if (option !== undefined) onOpenHit(option.session, option.hit);
  };

  const building = readiness !== null && readiness !== "ready";
  const sentence = stateSentence({
    query: feed.query,
    error: feed.error,
    readiness,
    progress,
    canonicalReady,
    ingestPaused,
    counts: results?.counts ?? null,
    limit: SEARCH_LIMIT,
  });
  const meter =
    building && progress !== null && progress.documents_total > 0
      ? `${Math.min(100, Math.max(2, (100 * progress.documents_done) / progress.documents_total)).toFixed(1)}%`
      : building
        ? "8%"
        : null;
  const scopeLine =
    agentKinds.length === 0 ? "your whole agentic past" : `across ${agentKinds.join(" · ")}`;
  const activeId = emptyQuery
    ? active >= 0 && recent[active] !== undefined
      ? `ah-sheet-recent-${active}`
      : undefined
    : options[active]?.id;

  const scopeOptions = useMemo(() => {
    if (currentProject === null) return null;
    const tint = { "--tint": currentProject.tint } as TintStyle;
    return [
      { value: "everywhere" as SearchScopeFilter, label: "everywhere" },
      {
        value: "project" as SearchScopeFilter,
        label: (
          <>
            <i style={tint} aria-hidden="true" />
            {currentProject.row.name}
          </>
        ),
        accessibleLabel: `only ${currentProject.row.name}`,
      },
    ];
  }, [currentProject]);

  let body: ReactNode;
  if (emptyQuery) {
    body = (
      <>
        {recent.length > 0 && (
          <>
            <div className="ah-sheet__recent-head" role="presentation">
              Recent searches
            </div>
            {recent.map((remembered, index) => (
              <div
                key={remembered}
                id={`ah-sheet-recent-${index}`}
                role="option"
                aria-selected={index === active}
                className="ah-sheet__recent"
                onPointerMove={() => hover(index)}
                onClick={() => onQuery(remembered)}
              >
                <SearchGlyph />
                <span>{remembered}</span>
                <button
                  type="button"
                  className="ah-sheet__forget"
                  aria-label={`Forget ${remembered}`}
                  tabIndex={-1}
                  onClick={(event) => {
                    event.stopPropagation();
                    setRecent(forgetQuery(safeStorage(), remembered));
                  }}
                >
                  ×
                </button>
              </div>
            ))}
          </>
        )}
        <Quiet title={recent.length > 0 ? "Or search for something new" : "Search your whole agentic past"}>
          {GRAMMAR_HINT}
        </Quiet>
      </>
    );
  } else if (lines.length === 0) {
    body = feed.error !== null ? (
      <Quiet title="Search failed">{feed.error}</Quiet>
    ) : building && results === null ? (
      <Quiet title={canonicalReady ? "Search is still being prepared" : "History is still being imported"}>
        {sentence}
      </Quiet>
    ) : results !== null ? (
      <Quiet title="Nothing matched">
        No indexed message contains every word of <code>{feed.query}</code>. Try fewer words, or a
        phrase in quotes.
      </Quiet>
    ) : null;
  } else {
    // One labelled group per session: the head names it for assistive
    // technology, and bounding the sticky head to its group is what lets the
    // next session's head push it away instead of painting over it.
    const groups: { head: SheetHead; rows: SheetOption[] }[] = [];
    for (const line of lines) {
      if (line.type === "head") groups.push({ head: line, rows: [] });
      else groups[groups.length - 1]?.rows.push(line);
    }
    body = groups.map(({ head, rows }) => (
      <div
        key={head.key}
        role="group"
        aria-labelledby={headDomId(head.session.id)}
        className="ah-sheet__group"
      >
        <HeadLine head={head} runtimeKind={getRuntimeKind(head.session.runtime_id)} nowMs={nowMs} />
        {rows.map((line) => (
          <OptionRow
            key={line.key}
            option={line}
            active={line.index === active}
            home={homeDirectory}
            nowMs={nowMs}
            runtimeKind={getRuntimeKind(line.session.runtime_id)}
            onHover={() => hover(line.index)}
            onOpen={() => onOpenHit(line.session, line.hit)}
          />
        ))}
      </div>
    ));
  }

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <Dialog.Portal>
        {/* `data-ah-sheet` is what the room's stylesheet watches to recede
            the stage and to stop the strip from dragging under the sheet. */}
        <Dialog.Overlay className="ah-sheet__backdrop" data-ah-sheet="" />
        <Dialog.Content
          className="ah-sheet"
          aria-describedby={undefined}
          onOpenAutoFocus={(event) => {
            // The field takes focus with its remembered text selected, so
            // typing replaces the last search and the arrows edit it.
            event.preventDefault();
            const input = inputRef.current;
            input?.focus();
            input?.select();
          }}
        >
          <Dialog.Title className="ah-visually-hidden">Search your whole agentic past</Dialog.Title>
          <div className="ah-sheet__field">
            <SearchGlyph />
            <input
              ref={inputRef}
              type="text"
              enterKeyHint="search"
              role="combobox"
              aria-expanded="true"
              aria-controls={LIST_ID}
              aria-activedescendant={activeId}
              aria-autocomplete="list"
              aria-label="Search your whole agentic past"
              placeholder="Search your whole agentic past"
              autoComplete="off"
              spellCheck={false}
              value={query}
              onChange={(event) => onQuery(event.target.value)}
              onKeyDown={onKeyDown}
            />
            {query !== "" && (
              <button
                type="button"
                className="ah-sheet__clear"
                aria-label="Clear the search"
                onClick={() => {
                  onQuery("");
                  inputRef.current?.focus();
                }}
              >
                ×
              </button>
            )}
            <kbd className="ah-kbd" aria-hidden="true">
              esc
            </kbd>
          </div>
          <div className="ah-sheet__scope">
            {scopeOptions !== null && (
              <SegmentedControl<SearchScopeFilter>
                value={filters.scope}
                options={scopeOptions}
                accessibleLabel="Where to search"
                onValueChange={(scope) => narrow({ ...filters, scope })}
              />
            )}
            <SegmentedControl<SearchKindFilter>
              value={filters.kind}
              options={KIND_FILTER_OPTIONS}
              accessibleLabel="Which kind of message"
              onValueChange={(kind) => narrow({ ...filters, kind })}
            />
            {agentKinds.length > 1 && (
              <select
                className="ah-chipselect"
                data-set={filters.agent !== null || undefined}
                aria-label="Which agent"
                value={filters.agent ?? ""}
                onChange={(event) =>
                  narrow({
                    ...filters,
                    agent: event.target.value === "" ? null : event.target.value,
                  })
                }
              >
                <option value="">any agent</option>
                {agentKinds.map((kind) => (
                  <option key={kind} value={kind}>
                    {agentLabel(kind)}
                  </option>
                ))}
              </select>
            )}
          </div>
          <div
            ref={listRef}
            className="ah-sheet__list"
            id={LIST_ID}
            role="listbox"
            aria-label={emptyQuery ? "Recent searches" : "Search results"}
            data-stale={feed.searching && results !== null ? "" : undefined}
          >
            {body}
          </div>
          <div className="ah-sheet__foot">
            {meter !== null && (
              <div className="ah-meter" aria-hidden="true">
                <i style={{ width: meter }} />
              </div>
            )}
            <span
              className="ah-sheet__count"
              role="status"
              aria-live="polite"
              data-tone={feed.error !== null ? "warn" : undefined}
            >
              {feed.searching && results === null && !emptyQuery ? "searching…" : sentence}
            </span>
            <span className="ah-sheet__scopeline" title={scopeLine}>
              {scopeLine}
            </span>
            <span className="ah-sheet__keys" aria-hidden="true">
              <span>
                <kbd className="ah-kbd">↑↓</kbd> move
              </span>
              <span>
                <kbd className="ah-kbd">↵</kbd> open
              </span>
              <span>
                <kbd className="ah-kbd">esc</kbd> close
              </span>
            </span>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
