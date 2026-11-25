// The worktree list as a ledger of disk: who holds how much, heaviest first.
// Pure, so the order and the bars are testable without a window.

import type { GitWorktree } from "../../ports";

/** One drawn row. */
export interface LedgerRow {
  readonly path: string;
  /** What the row says and what a press copies: the branch, or for a checkout
   * with no branch its folder's own name. */
  readonly name: string;
  /** The commit beside a folder's name, abbreviated; null beside a branch,
   * which already names what is checked out. */
  readonly commit: string | null;
  readonly open: boolean;
  readonly gone: boolean;
  /** null: being measured. undefined: nobody measures here. */
  readonly sizeBytes: number | null | undefined;
  /** The bar: this row's share of the heaviest row, 0 to 1. null with no figure. */
  readonly share: number | null;
}

export interface Ledger {
  readonly rows: readonly LedgerRow[];
  /** The sum of the figures there are; null while there is none. */
  readonly totalBytes: number | null;
  /** True while some row that will have a figure has none yet, so the sum
   * is a floor. */
  readonly partial: boolean;
}

/** git's abbreviation length, the one `git log --oneline` prints. */
const COMMIT_CHARS = 7;

function folderName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const at = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  const name = at === -1 ? trimmed : trimmed.slice(at + 1);
  return name === "" ? path : name;
}

/** The rows in the order they are drawn: measured ones heaviest first, then
 * the ones still being measured in git's own order, then the ones whose folder
 * is gone. With no figures at all it is git's order, which is the list this
 * replaced. A sort that kept unmeasured rows where git put them would move a
 * row twice: once when its neighbour's figure arrived, once for its own. */
export function ledgerOf(worktrees: readonly GitWorktree[], openRoot: string | null): Ledger {
  const measured = worktrees.filter((w) => w.gone !== true && typeof w.sizeBytes === "number");
  const heaviest = measured.reduce((most, w) => Math.max(most, w.sizeBytes as number), 0);
  const rank = (w: GitWorktree): number => (w.gone === true ? 2 : typeof w.sizeBytes === "number" ? 0 : 1);

  const rows = worktrees
    .map((worktree, at) => ({ worktree, at }))
    .sort((a, b) => {
      const byRank = rank(a.worktree) - rank(b.worktree);
      if (byRank !== 0) return byRank;
      if (rank(a.worktree) === 0) {
        const bySize = (b.worktree.sizeBytes as number) - (a.worktree.sizeBytes as number);
        if (bySize !== 0) return bySize;
      }
      return a.at - b.at;
    })
    .map(({ worktree }): LedgerRow => {
      const gone = worktree.gone === true;
      const size = gone ? undefined : worktree.sizeBytes;
      return {
        path: worktree.path,
        name: worktree.branch ?? folderName(worktree.path),
        commit:
          worktree.branch === null && typeof worktree.head === "string" && worktree.head !== ""
            ? worktree.head.slice(0, COMMIT_CHARS)
            : null,
        open: worktree.path === openRoot,
        gone,
        sizeBytes: size,
        share: typeof size === "number" ? (heaviest > 0 ? size / heaviest : 0) : null,
      };
    });

  return {
    rows,
    totalBytes: measured.length === 0 ? null : measured.reduce((sum, w) => sum + (w.sizeBytes as number), 0),
    partial: worktrees.some((w) => w.gone !== true && w.sizeBytes === null),
  };
}
