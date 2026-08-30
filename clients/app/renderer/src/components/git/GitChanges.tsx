// The Changes view: what the index holds, and what the working tree holds.
//
// Two groups, not a mark on every row — "it is the shape `git status` itself
// has… and position is the quietest way to say a binary" (design record §8).
// A file with staged changes AND further edits appears in both, because it is
// in both, and the two rows open two different diffs.

import { useMemo } from "react";
import { EmptyState } from "../ui";
import { ChangeTree } from "./ChangeTree";
import { buildPathTree, filesUnder } from "./path-tree";
import type { TreeRow } from "./path-tree";
import type { StatusEntry, StatusLetter } from "./types";

export interface GitChangesProps {
  readonly entries: readonly StatusEntry[];
  readonly sessionPaths: ReadonlySet<string>;
  readonly selection: { readonly path: string; readonly staged: boolean } | null;
  readonly onSelect: (path: string, staged: boolean) => void;
  readonly onStage: ((paths: readonly string[]) => void) | null;
  readonly onUnstage: ((paths: readonly string[]) => void) | null;
  readonly onDiscard: ((paths: readonly string[]) => void) | null;
}

interface Group {
  readonly rows: readonly TreeRow[];
  readonly letters: ReadonlyMap<string, StatusLetter>;
  readonly count: number;
}

function group(entries: readonly StatusEntry[], slot: "index" | "worktree"): Group {
  const letters = new Map<string, StatusLetter>();
  for (const entry of entries) {
    const letter = entry[slot];
    if (letter !== null) letters.set(entry.path, letter);
  }
  return {
    rows: buildPathTree([...letters.keys()]),
    letters,
    count: letters.size,
  };
}

export function GitChanges({
  entries,
  sessionPaths,
  selection,
  onSelect,
  onStage,
  onUnstage,
  onDiscard,
}: GitChangesProps) {
  const staged = useMemo(() => group(entries, "index"), [entries]);
  const unstaged = useMemo(() => group(entries, "worktree"), [entries]);

  if (staged.count === 0 && unstaged.count === 0) {
    return (
      <EmptyState
        title="Nothing to commit"
        description="The working tree matches HEAD. Every change this branch has is in its history."
      />
    );
  }

  return (
    <div className="ah-scroll">
      {staged.count > 0 && (
        <>
          <div className="ah-group">
            Staged
            <span className="ah-hitcount tnum">{staged.count}</span>
          </div>
          <ChangeTree
            rows={staged.rows}
            letters={staged.letters}
            sessionPaths={sessionPaths}
            selectedPath={selection?.staged === true ? selection.path : null}
            onSelect={(row) => row.kind === "file" && onSelect(row.path, true)}
            action={
              onUnstage === null
                ? null
                : { label: "Unstage", run: (row) => onUnstage(filesUnder(staged.rows, row)) }
            }
            onDiscard={null}
          />
        </>
      )}
      {unstaged.count > 0 && (
        <>
          <div className="ah-group">
            Unstaged
            <span className="ah-hitcount tnum">{unstaged.count}</span>
          </div>
          <ChangeTree
            rows={unstaged.rows}
            letters={unstaged.letters}
            sessionPaths={sessionPaths}
            selectedPath={selection?.staged === false ? selection.path : null}
            onSelect={(row) => row.kind === "file" && onSelect(row.path, false)}
            action={
              onStage === null
                ? null
                : { label: "Stage", run: (row) => onStage(filesUnder(unstaged.rows, row)) }
            }
            // Discard is the one act here that destroys work, so it is offered
            // only on the unstaged side — the side where "throw this away" is
            // the whole of what it means.
            onDiscard={
              onDiscard === null ? null : (row) => onDiscard(filesUnder(unstaged.rows, row))
            }
          />
        </>
      )}
    </div>
  );
}
