// Column ③ — the project's worktree, opened from the session column and
// sitting immediately to its right, so a tree is always beside its owner
// rather than across the room from it (design record §8).
//
// The column is always in the grid: closed, it is a 0px track, and the
// measure takes back whatever the guest gives up. Which is why this reads as
// three states rather than as "render or don't".
//
// It also owns which file is open. The viewer beside it knows nothing about
// engines or projects — it takes a name, a size and something that produces
// bytes — so this is the one place the two halves are joined.

import { useEffect, useMemo, useState } from "react";
import type { Entry } from "@vibecook/mille";
import { desktopHost } from "../../lib/desktop-host";
import { FileViewerDialog } from "../file-viewer";
import type { FileHandle } from "../file-viewer";
import { EmptyState } from "../ui";
import { useGitFacts, useWorktreeEngine } from "./worktree-host";
import { createWorktreeFileSource, handleFor, isOpenable } from "./worktree-file-source";
import { WorktreeFileTree } from "./WorktreeFileTree";
import { WorktreeHeader } from "./WorktreeHeader";
import { WorktreeList } from "./WorktreeList";

export interface WorktreeColumnProps {
  /** The selected project's first root path, from rosettad. */
  readonly projectRoot: string | null;
  /** The project's §2.6 accent, following it across the room. */
  readonly tint: string | null;
  readonly homeDirectory: string | null;
  /** The room's receipt line, so an act inside the viewer is never silent. */
  readonly onReceipt?: (text: string) => void;
}

export function WorktreeColumn({
  projectRoot,
  tint,
  homeDirectory,
  onReceipt,
}: WorktreeColumnProps) {
  const engine = useWorktreeEngine();
  // The engine follows the selection with a lag (kill, re-fork, walk), so the
  // head names what the tree is actually showing, never what was asked for.
  const shownRoot = engine?.root ?? null;
  const facts = useGitFacts(shownRoot);

  const hasShell = desktopHost() !== null;
  const stale = projectRoot !== null && shownRoot !== null && shownRoot !== projectRoot;
  const hasSiblingWorktrees = facts !== null && facts.worktrees.length > 1;

  const [open, setOpen] = useState<FileHandle | null>(null);
  // One source per engine: the read hook treats a new source as a new
  // request, and a project switch invalidates every id the old one issued.
  const source = useMemo(
    () => (engine === null ? null : createWorktreeFileSource(engine)),
    [engine],
  );
  useEffect(() => setOpen(null), [source]);

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
      ) : engine === null || source === null ? (
        <EmptyState
          title="Opening the worktree"
          description="The engine is walking the project root."
        />
      ) : (
        <>
          <WorktreeFileTree
            engine={engine}
            stale={stale}
            openEntryId={typeof open?.ref === "number" ? open.ref : null}
            onOpen={(entry: Entry) => {
              // A folder toggles rather than opens; mille only reports rows
              // with no children, which an empty directory also is.
              if (isOpenable(entry)) setOpen(handleFor(engine, entry));
            }}
          />
          {hasSiblingWorktrees && (
            <WorktreeList facts={facts} openRoot={shownRoot} homeDirectory={homeDirectory} />
          )}
          <FileViewerDialog
            handle={open}
            source={source}
            tint={tint}
            homeDirectory={homeDirectory}
            onClose={() => setOpen(null)}
            onReceipt={onReceipt}
            openInBrowser={desktopHost()?.openInBrowser ?? null}
          />
        </>
      )}
    </div>
  );
}
