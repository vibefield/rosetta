// The repository's other checkouts, under the tree that is showing one of
// them. Only worth drawing when there is more than one: a repository with a
// single worktree has nothing to disambiguate, and a list of one is a label.
//
// It is a ledger of disk. A repository's worktrees hold the same tracked
// files, so what tells them apart on a disk is what git does not track (the
// installed packages, the build output), and a reader deciding which checkout
// to let go of wants that figure first. So the rows stand heaviest first,
// each with a bar scaled to the heaviest and its size at the edge; where the
// folder is and how dirty the open one is are on the row's hover.
//
// A row is named by its branch, or, with no branch, by its folder's own name
// with its commit beside it. The whole path of a branchless checkout was four
// lines of this column, with the one part that told two apart at the very end.
//
// A press copies the name. That is the room's one act here: switching
// worktrees is an act on a repository, and this room reads history.

import type { GitFacts } from "../../ports";
import { fmtBytes, tildePath } from "../../lib/format";
import { CopyGlyph } from "../../lib/icons";
import { ledgerOf } from "./worktree-ledger";

export interface WorktreeListProps {
  readonly facts: GitFacts;
  /** The root the tree is showing, which earns the dirty count. */
  readonly openRoot: string | null;
  readonly homeDirectory: string | null;
  /** The room's receipt line, so a copy is never silent. */
  readonly onReceipt: (text: string) => void;
}

export function WorktreeList({ facts, openRoot, homeDirectory, onReceipt }: WorktreeListProps) {
  const ledger = ledgerOf(facts.worktrees, openRoot);
  // `ah-rail*` is the design record's name for this column; room.css is a
  // port of the proto and keeps its vocabulary.
  return (
    <div className="ah-railfoot">
      <div className="ah-railsplit">
        <span className="ah-eyebrow">Worktrees</span>
        <span className="ah-railsplit__sum tnum">
          {ledger.rows.length}
          {ledger.totalBytes !== null && ` · ${fmtBytes(ledger.totalBytes)}${ledger.partial ? "+" : ""}`}
        </span>
      </div>
      <div className="ah-railfoot__list" role="list" aria-label="Worktrees">
        {ledger.rows.map((row) => {
          // The count belongs to the checkout the shell actually read, and
          // git reports it for one worktree at a time.
          const dirty = row.open ? facts.dirty : null;
          const where = tildePath(row.path, homeDirectory);
          const hover = [
            where,
            row.gone ? "its folder is gone" : null,
            dirty === null ? null : dirty > 0 ? `${dirty} dirty` : "clean",
            "Press to copy the name",
          ]
            .filter((part) => part !== null)
            .join(" · ");
          return (
            <div role="listitem" key={row.path}>
              <button
                type="button"
                className="ah-worktree"
                title={hover}
                data-open={row.open ? "" : undefined}
                data-gone={row.gone ? "" : undefined}
                onClick={() => {
                  void navigator.clipboard
                    .writeText(row.name)
                    .then(() => onReceipt("worktree name copied"))
                    .catch(() => onReceipt("the clipboard refused"));
                }}
              >
                <span className="ah-worktree__who">
                  <span className="ah-worktree__name">
                    <span className="ah-worktree__label">{row.name}</span>
                    {row.commit !== null && <code>{row.commit}</code>}
                    <span className="ah-worktree__copy">
                      <CopyGlyph />
                    </span>
                  </span>
                  {row.share !== null && (
                    <span className="ah-worktree__bar" aria-hidden="true">
                      <span style={{ width: `${Math.max(row.share * 100, 1.5).toFixed(2)}%` }} />
                    </span>
                  )}
                </span>
                <span className="ah-worktree__size tnum">
                  {row.gone
                    ? "gone"
                    : typeof row.sizeBytes === "number"
                      ? fmtBytes(row.sizeBytes)
                      : row.sizeBytes === null
                        ? "…"
                        : ""}
                </span>
              </button>
            </div>
          );
        })}
      </div>
    </div>
  );
}
