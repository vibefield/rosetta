// Column ③ — the project's worktree, opened from the session column and
// sitting immediately to its right, so a tree is always beside its owner
// rather than across the room from it (design record §8).
//
// The column is always in the grid: closed, it is a 0px track, and the
// measure takes back whatever the guest gives up. Which is why this reads as
// three states rather than as "render or don't".

import { desktopHost } from "../../lib/desktop-host";
import { EmptyState } from "../ui";
import { useGitFacts, useWorktreeEngine } from "./worktree-host";
import { WorktreeFileTree } from "./WorktreeFileTree";
import { WorktreeHeader } from "./WorktreeHeader";
import { WorktreeList } from "./WorktreeList";

export interface WorktreeColumnProps {
  /** The selected project's first root path, from rosettad. */
  readonly projectRoot: string | null;
  /** The project's §2.6 accent, following it across the room. */
  readonly tint: string | null;
  readonly homeDirectory: string | null;
}

export function WorktreeColumn({ projectRoot, tint, homeDirectory }: WorktreeColumnProps) {
  const engine = useWorktreeEngine();
  // The engine follows the selection with a lag (kill, re-fork, walk), so the
  // head names what the tree is actually showing, never what was asked for.
  const shownRoot = engine?.root ?? null;
  const facts = useGitFacts(shownRoot);

  const hasShell = desktopHost() !== null;
  const stale = projectRoot !== null && shownRoot !== null && shownRoot !== projectRoot;
  const hasSiblingWorktrees = facts !== null && facts.worktrees.length > 1;

  return (
    <div className="ah-col ah-col--rail">
      <WorktreeHeader
        root={shownRoot}
        tint={tint}
        homeDirectory={homeDirectory}
        facts={facts}
      />
      {!hasShell ? (
        <EmptyState
          title="No worktree here"
          description="The tree needs the desktop shell — a browser tab has no engine to walk it."
        />
      ) : engine === null ? (
        <EmptyState
          title="Opening the worktree"
          description="The engine is walking the project root."
        />
      ) : (
        <>
          <WorktreeFileTree engine={engine} stale={stale} />
          {hasSiblingWorktrees && (
            <WorktreeList facts={facts} openRoot={shownRoot} homeDirectory={homeDirectory} />
          )}
        </>
      )}
    </div>
  );
}
