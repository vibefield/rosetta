// The head names the branch, because that is the object this column is a view
// of — and a branch is an id, so it sets in mono, the same rule the sha and
// the path follow.
//
// The badge counts what the column is LISTING; the caption states the shape
// of that set in one line — what git would say about it. Both are counted off
// the array the column is drawn from, so the drawing and the sentence about
// it cannot drift.

import { ColumnHeading } from "../ui";
import type { BranchFacts, Commit, GitView } from "./types";

export interface GitHeaderProps {
  readonly branch: BranchFacts | null;
  readonly tint: string | null;
  readonly view: GitView;
  /** What the column is listing right now. */
  readonly count: number;
  /** The caption for the Changes view: `2 added · 5 modified`. */
  readonly changeShape: string;
  readonly unpushed: number | null;
  /** The commit being read, when the column has drilled into one. The head
   * names the object the column is SHOWING, and while a commit is open that
   * object is the commit, not the branch. */
  readonly commit: Commit | null;
  readonly onBack: () => void;
}

/** `3 unpushed`, or the honest silence of a branch with no upstream. */
export function formatSync(branch: BranchFacts | null, unpushed: number | null): string {
  if (branch === null) return "";
  if (branch.upstream === null) {
    // Not "0 unpushed": with no upstream, nothing here knows.
    return "no upstream";
  }
  const parts: string[] = [];
  if (unpushed !== null && unpushed > 0) parts.push(`${unpushed} unpushed`);
  if (branch.behind !== null && branch.behind > 0) parts.push(`${branch.behind} behind`);
  return parts.length > 0 ? parts.join(" · ") : "up to date";
}

export function GitHeader({
  branch,
  tint,
  view,
  count,
  changeShape,
  unpushed,
  commit,
  onBack,
}: GitHeaderProps) {
  const name = branch?.name ?? null;
  const caption = view === "history" ? formatSync(branch, unpushed) : changeShape;

  if (commit !== null) {
    return (
      <header>
        <div className="ah-backhead">
          <button
            type="button"
            className="ah-back"
            onClick={onBack}
            // Names where it goes, not just that it goes back — the branch is
            // the object the reader came from.
            title={`Back to ${name ?? "the branch"}`}
            aria-label={`Back to ${name ?? "the branch"}`}
          >
            ‹
          </button>
          <ColumnHeading
            name={commit.short}
            tint={tint}
            mono
            count={count}
            countTitle={`${count} ${count === 1 ? "file" : "files"}`}
            title={commit.sha}
          />
        </div>
        <span className="ah-caption tnum">
          {commit.merge ? "merge" : `+${commit.added} −${commit.deleted}`}
        </span>
      </header>
    );
  }

  return (
    <header>
      <ColumnHeading
        name={name ?? "—"}
        tint={tint}
        mono
        count={count}
        countTitle={`${count} ${view === "history" ? "commits" : "changed files"}`}
        title={branch?.upstream !== null && branch?.upstream !== undefined
          ? `${name} → ${branch.upstream}`
          : name}
      />
      {caption !== "" && <span className="ah-caption tnum">{caption}</span>}
    </header>
  );
}
