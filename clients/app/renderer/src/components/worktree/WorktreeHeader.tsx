// The column is named by the folder it is showing, because that is the thing
// — "Worktree" was a category, and the head names objects. A path is an id,
// so it sets in mono.
//
// No count. We know how many files the tree is SHOWING, not how many the
// repository has, and a badge counting the interface is not a fact — which
// is the one thing this head does differently from its three siblings.

import type { GitFacts } from "../../lib/desktop-host";
import { tildePath } from "../../lib/format";
import { ColumnHeading } from "../ui";

export interface WorktreeHeaderProps {
  /** The root the tree is actually showing, or null before one opens. */
  readonly root: string | null;
  /** The project's §2.6 accent, or null when nothing is selected. */
  readonly tint: string | null;
  readonly homeDirectory: string | null;
  readonly facts: GitFacts | null;
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

export function WorktreeHeader({ root, tint, homeDirectory, facts }: WorktreeHeaderProps) {
  const caption = formatGitFacts(facts);
  return (
    <header>
      <ColumnHeading
        name={root === null ? "—" : tildePath(root, homeDirectory)}
        tint={tint}
        mono
        // The head abbreviates to `~`; the tooltip keeps the whole path.
        title={root}
      />
      {caption !== null && <span className="ah-caption tnum">{caption}</span>}
    </header>
  );
}
