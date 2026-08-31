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
// the column it described, which was scoped to ONE session. This one is a
// view of the PROJECT's repository and lists commits from sessions that are
// not in this room at all — most of them from no session it has ever seen.
// The session row cannot speak for them, so the redundancy the rule guarded
// against does not exist here, and the mark is the fastest way to read a
// history for who built it. It follows §2.6's rule for provider identity:
// monochrome, at `currentColor`, carrying no hue of its own.
//
// The record also says the row is not pressable, "this proposal has no diff
// view, and a hover fill would be promising one". There is a diff view now,
// so the row is pressable and the promise is kept.

import { EmptyState } from "../ui";
import { AgentMark } from "../../lib/agents";
import { lastActive } from "../../lib/format";
import { creditedAgents } from "./co-author";
import type { Commit } from "./types";

/**
 * Everyone git says wrote this commit, minus the agents the row is already
 * drawing as marks.
 *
 * A recognised agent is a glyph at the end of the message line; naming it
 * here as well would spend a 280px row saying the same thing twice. An
 * UNRECOGNISED co-author is a person, and keeps their name.
 */
function byline(commit: Commit): string {
  const drawn = new Set(creditedAgents(commit.coAuthors).map((agent) => agent.label));
  return [commit.author, ...commit.coAuthors.map((author) => author.name)]
    .filter((name) => name !== "" && !drawn.has(name))
    .join(", ");
}

export interface GitHistoryProps {
  readonly commits: readonly Commit[];
  /** The commit the column has opened, so the row it came from stays lit. */
  readonly selectedSha: Commit | null;
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
      {commits.map((commit) => {
        const agents = creditedAgents(commit.coAuthors);
        return (
        <button
          type="button"
          className="ah-commit"
          key={commit.sha}
          data-selected={commit.sha === selectedSha?.sha ? "" : undefined}
          onClick={() => onSelect(commit)}
        >
          {commit.pushed === false && <PushMark />}
          <span className="ah-commit__l1">
            <strong title={commit.subject}>{commit.subject}</strong>
            {agents.length > 0 && (
              <span className="ah-agents ah-commit__agents">
                {agents.map((agent) => (
                  <AgentMark key={agent.kind} kind={agent.kind} note={agent.label} />
                ))}
              </span>
            )}
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
        );
      })}
    </div>
  );
}
