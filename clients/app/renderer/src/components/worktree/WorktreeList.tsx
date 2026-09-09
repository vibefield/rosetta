// The repository's other checkouts, under the tree that is showing one of
// them. Only worth drawing when there is more than one: a repository with a
// single worktree has nothing to disambiguate, and a list of one is a label.
//
// Rows are not buttons. Switching worktrees is an act on a repository, and
// this room reads history — so the row states which checkout is open and how
// dirty it is, and stops there.

import type { GitFacts } from "../../lib/desktop-host";
import { tildePath } from "../../lib/format";

export interface WorktreeListProps {
  readonly facts: GitFacts;
  /** The root the tree is showing, which earns the dirty count. */
  readonly openRoot: string | null;
  readonly homeDirectory: string | null;
}

export function WorktreeList({ facts, openRoot, homeDirectory }: WorktreeListProps) {
  // `ah-rail*` is the design record's name for this column; room.css is a
  // port of the proto and keeps its vocabulary.
  return (
    <div className="ah-railfoot">
      <div className="ah-railsplit">
        <span className="ah-eyebrow">Worktrees</span>
      </div>
      <div className="ah-railfoot__list" role="region" aria-label="Worktrees" tabIndex={0}>
        {facts.worktrees.map((worktree) => {
          // The count belongs to the checkout the shell actually read, and
          // git reports it for one worktree at a time.
          const dirty = worktree.path === openRoot ? facts.dirty : null;
          return (
            <div className="ah-worktree" key={worktree.path} title={worktree.path}>
              <span>{worktree.branch ?? tildePath(worktree.path, homeDirectory)}</span>
              {dirty !== null && (
                <code data-dirty={dirty > 0 ? "" : undefined} className="tnum">
                  {dirty > 0 ? `${dirty} dirty` : "clean"}
                </code>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
