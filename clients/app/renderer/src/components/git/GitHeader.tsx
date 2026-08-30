// The head names the branch, because that is the object this column is a view
// of — and a branch is an id, so it sets in mono, the same rule the sha and
// the path follow.
//
// The badge counts what the column is LISTING; the caption states the shape
// of that set in one line — what git would say about it. Both are counted off
// the array the column is drawn from, so the drawing and the sentence about
// it cannot drift.

import { ColumnHeading } from "../ui";
import type { BranchFacts, GitView } from "./types";

export interface GitHeaderProps {
  readonly branch: BranchFacts | null;
  readonly tint: string | null;
  readonly view: GitView;
  /** What the column is listing right now. */
  readonly count: number;
  /** The caption for the Changes view: `2 added · 5 modified`. */
  readonly changeShape: string;
  readonly unpushed: number | null;
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
}: GitHeaderProps) {
  const name = branch?.name ?? null;
  const caption = view === "history" ? formatSync(branch, unpushed) : changeShape;

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
