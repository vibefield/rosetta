// The History view: what this branch has committed.
//
// The commit row is the session row one object down — message on top, the
// byline and diffstat in the middle, sha bottom-left and age bottom-right —
// with two rules from the design record §8:
//
//   · the diffstat is NEUTRAL, never git's green and red. Hue in this room
//     says which project or which state; a direction of change is neither;
//   · only one git state is drawn — not pushed yet — because it is the one
//     the reader can still act on.
//
// The record's third rule was that the row carries no agent mark, because
// "which one ran this session is already on the session row". That held for
// the column it described, which was scoped to ONE session. This one is
// scoped to the repository and lists commits from sessions that are not in
// this room at all, so the session row cannot speak for them and the
// redundancy the rule guarded against is gone.
//
// What it still does NOT get is the agent GLYPH — that is the provider
// iconography the rule names, and it would be a second vocabulary for a fact
// git already states in words. A co-author is shown the way git records it
// and the way every other git client shows it: as a name in the byline.
//
// The record also says the row is not pressable, "this proposal has no diff
// view, and a hover fill would be promising one". There is a diff view now,
// so the row is pressable and the promise is kept.

import { EmptyState } from "../ui";
import { lastActive } from "../../lib/format";
import type { Commit } from "./types";

/** Everyone git says wrote this commit: the author who signed it, then the
 * co-authors it names — which in this room is usually the agent. */
function byline(commit: Commit): string {
  return [commit.author, ...commit.coAuthors.map((author) => author.name)]
    .filter((name) => name !== "")
    .join(", ");
}

export interface GitHistoryProps {
  readonly commits: readonly Commit[];
  readonly selectedSha: string | null;
  readonly onSelect: (commit: Commit) => void;
  readonly currentTimestamp: number;
}

/** The unpushed mark: an arrow out, in the row's marker column. */
function PushMark() {
  return (
    <i className="ah-push" title="not pushed yet">
      <svg
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        <path d="M8 13V3.6" />
        <path d="M3.8 7.8 8 3.4l4.2 4.4" />
      </svg>
    </i>
  );
}

export function GitHistory({
  commits,
  selectedSha,
  onSelect,
  currentTimestamp,
}: GitHistoryProps) {
  if (commits.length === 0) {
    return (
      <EmptyState
        title="No commits yet"
        description="This branch has no history of its own to show."
      />
    );
  }

  return (
    <div className="ah-scroll">
      {commits.map((commit) => (
        <button
          type="button"
          className="ah-commit"
          key={commit.sha}
          data-selected={commit.sha === selectedSha ? "" : undefined}
          onClick={() => onSelect(commit)}
        >
          {commit.pushed === false && <PushMark />}
          <span className="ah-commit__l1">
            <strong title={commit.subject}>{commit.subject}</strong>
          </span>
          <span className="ah-commit__meta">
            <span className="ah-commit__who" title={byline(commit)}>
              {byline(commit)}
            </span>
            <span>·</span>
            {commit.merge ? (
              // A merge's diffstat is not a fact about one person's work.
              <span>merge</span>
            ) : (
              <>
                <span className="tnum">
                  +{commit.added} −{commit.deleted}
                </span>
                <span>·</span>
                <span className="tnum">
                  {commit.files} {commit.files === 1 ? "file" : "files"}
                </span>
              </>
            )}
          </span>
          <span className="ah-commit__l3">
            <code>{commit.short}</code>
            <span className="ah-last">{lastActive(commit.timestampMs, currentTimestamp)}</span>
          </span>
        </button>
      ))}
    </div>
  );
}
