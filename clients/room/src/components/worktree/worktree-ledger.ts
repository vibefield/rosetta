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
  /** Bytes; null while the figure is owed (asked for, or to be asked once the
   * row is on screen); undefined when there will be none. */
  readonly sizeBytes: number | null | undefined;
  /** The bar: this row's share of the heaviest row, 0 to 1. null with no figure. */
  readonly share: number | null;
}

export interface Ledger {
  /** Measured rows heaviest first, then the ones owed a figure in git's own
   * order, then the ones whose folder is gone. */
  readonly rows: readonly LedgerRow[];
  /** The sum of the figures there are; null while there is none. */
  readonly totalBytes: number | null;
  /** True while some row is owed a figure, so the sum is a floor. */
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

/** The ledger over what git listed. `sizeOf` says each worktree's figure as
 * the list knows it now (the host's kept figure, or a later answer): bytes,
 * null while owed, undefined when there will be none. Absent, the figure is
 * the one the worktree came with. */
export function ledgerOf(
  worktrees: readonly GitWorktree[],
  openRoot: string | null,
  sizeOf: (worktree: GitWorktree) => number | null | undefined = (w) => w.sizeBytes,
): Ledger {
  const sized = worktrees.map((worktree, at) => ({
    worktree,
    at,
    gone: worktree.gone === true,
    size: worktree.gone === true ? undefined : sizeOf(worktree),
  }));
  const heaviest = sized.reduce(
    (most, row) => (typeof row.size === "number" ? Math.max(most, row.size) : most),
    0,
  );
  const rank = (row: (typeof sized)[number]): number =>
    row.gone ? 2 : typeof row.size === "number" ? 0 : 1;

  const rows = [...sized]
    .sort((a, b) => {
      const byRank = rank(a) - rank(b);
      if (byRank !== 0) return byRank;
      if (rank(a) === 0) {
        const bySize = (b.size as number) - (a.size as number);
        if (bySize !== 0) return bySize;
      }
      return a.at - b.at;
    })
    .map(({ worktree, gone, size }): LedgerRow => ({
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
    }));

  const measured = sized.filter((row) => typeof row.size === "number");
  return {
    rows,
    totalBytes:
      measured.length === 0 ? null : measured.reduce((sum, row) => sum + (row.size as number), 0),
    partial: sized.some((row) => !row.gone && row.size === null),
  };
}

/** The order the rows are DRAWN in. The ledger's own order is by weight, but
 * figures arrive one at a time while the list is being read, and a row that
 * leaves for its rank the moment it is weighed takes the reader's place with
 * it. So the drawn order follows the ledger only while the list is at rest
 * (at its top, with no pointer or focus in it); otherwise each row stays
 * where it was, a row new to the list goes after them in the ledger's order,
 * and a row no longer listed goes. */
export function settleOrder(
  drawn: readonly string[],
  ledger: readonly string[],
  atRest: boolean,
): readonly string[] {
  if (atRest || drawn.length === 0) return ledger;
  const listed = new Set(ledger);
  const kept = drawn.filter((path) => listed.has(path));
  const seen = new Set(kept);
  return [...kept, ...ledger.filter((path) => !seen.has(path))];
}
