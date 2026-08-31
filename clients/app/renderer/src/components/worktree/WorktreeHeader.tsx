// The column is named by the folder it is showing, because that is the thing
// — "Worktree" was a category, and the head names objects. A path is an id,
// so it sets in mono.
//
// One column, three tabs, and the head names what THAT tab is a view of —
// which is a different object per tab, and the design record already draws
// it that way across the two columns this one merges: its rail head names
// the folder, its git head names the branch. Files is a view of a checkout,
// so it takes the path; changes and history are views of a ref, so they take
// the branch. Inside a commit the object is the commit.
//
// It is also what fits. A 280px head holding a path, a count and a
// three-term caption gave the path four characters, and a head that cannot
// say its object's name is not naming it.
//
// The caption is what git would say about that tab's set, and the badge
// counts what the tab is listing — which is why Files has none: we know how
// many files the tree is SHOWING, not how many the repository has, and a
// badge counting the interface is not a fact about the thing.

import { ColumnHeading } from "../ui";
import { tildePath } from "../../lib/format";
import type { GitFacts } from "../../lib/desktop-host";
import type { BranchFacts, Commit } from "../git";
import type { RepoTab } from "./types";

export interface WorktreeHeaderProps {
  /** The root the tree is actually showing, or null before one opens. */
  readonly root: string | null;
  readonly tint: string | null;
  readonly homeDirectory: string | null;
  /** Branch and dirty count from the shell, for the Files tab's caption. */
  readonly facts: GitFacts | null;
  /** Branch and upstream from the engine, for History's. */
  readonly branch: BranchFacts | null;
  readonly tab: RepoTab;
  /** What the current tab is listing, or null for a tab with no honest count. */
  readonly count: number | null;
  readonly changeShape: string;
  readonly unpushed: number | null;
  /** Set while the column has drilled into one commit. */
  readonly commit: Commit | null;
  readonly onBack: () => void;
}

/** `branch · n dirty` — and nothing at all without a branch to hang it on.
 * The dirty count is the change set's size, not the number of letters the
 * tree happens to be drawing: a file inside a collapsed folder is still
 * dirty, and reporting fewer than git would is reporting the interface. */
export function formatGitFacts(facts: GitFacts | null): string | null {
  if (facts === null || facts.branch === null) return null;
  if (facts.dirty === null) return facts.branch;
  return `${facts.branch} · ${facts.dirty} dirty`;
}

/** `3 unpushed`, or the honest silence of a branch with no upstream. */
export function formatSync(branch: BranchFacts | null, unpushed: number | null): string {
  if (branch === null) return "";
  // Not "0 unpushed": with no upstream, nothing here knows.
  if (branch.upstream === null) return "no upstream";
  const parts: string[] = [];
  if (unpushed !== null && unpushed > 0) parts.push(`${unpushed} unpushed`);
  if (branch.behind !== null && branch.behind > 0) parts.push(`${branch.behind} behind`);
  return parts.length > 0 ? parts.join(" · ") : "up to date";
}

export function WorktreeHeader({
  root,
  tint,
  homeDirectory,
  facts,
  branch,
  tab,
  count,
  changeShape,
  unpushed,
  commit,
  onBack,
}: WorktreeHeaderProps) {
  if (commit !== null) {
    return (
      <header>
        <div className="ah-backhead">
          <button
            type="button"
            className="ah-back"
            onClick={onBack}
            // Names where it goes, not just that it goes back.
            title="Back to the history"
            aria-label="Back to the history"
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

  const caption =
    tab === "history"
      ? formatSync(branch, unpushed)
      : tab === "changes"
        ? changeShape
        : (formatGitFacts(facts) ?? "");

  const onFiles = tab === "files";
  const path = root === null ? "—" : tildePath(root, homeDirectory);
  // A detached HEAD has no branch name; the checkout it is in is the honest
  // fallback, and it is the same object either way.
  const ref = branch?.name ?? facts?.branch ?? path;

  return (
    <header>
      <ColumnHeading
        name={onFiles ? path : ref}
        tint={tint}
        mono
        count={count}
        countTitle={
          count === null
            ? undefined
            : `${count} ${tab === "history" ? "commits" : "changed files"}`
        }
        // Files abbreviates to `~`, so the tooltip keeps the whole path; a
        // branch on the other two is already whole, and its tooltip says
        // which checkout it is a branch OF.
        title={onFiles ? root : (root ?? undefined)}
      />
      {caption !== "" && <span className="ah-caption tnum">{caption}</span>}
    </header>
  );
}
