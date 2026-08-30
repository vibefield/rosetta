// What column ⑤ is a view of.
//
// The column is scoped to the REPOSITORY, like every other git client — and
// the files this session touched wear a mark. The design record's §8 offered
// the opposite (a column scoped to one session, Q4's "keep or kill"); this
// keeps the idea it was after and drops the filter, because a list that hides
// the other twenty changed files is not a thing you can commit from, and the
// session attribution is the one fact GitHub Desktop could never show.

/** git's own status letter, per slot. `?` is untracked and `U` unmerged. */
export type StatusLetter = "M" | "A" | "D" | "R" | "C" | "T" | "?" | "U";

/**
 * One path's status, with BOTH of git's slots kept.
 *
 * A file with staged changes and further unstaged edits is genuinely in both
 * groups, with a different diff in each. Collapsing the pair into one letter
 * is right for a row decoration and wrong for a list you commit from.
 */
export interface StatusEntry {
  /** Workspace-relative, POSIX separators — what git prints. */
  readonly path: string;
  /** Where a rename came from. */
  readonly original: string | null;
  /** The index against HEAD: what `git commit` would record. */
  readonly index: StatusLetter | null;
  /** The working tree against the index: what is not staged yet. */
  readonly worktree: StatusLetter | null;
}

export interface BranchFacts {
  readonly name: string | null;
  readonly upstream: string | null;
  /** Commits this branch has that its upstream does not. `null` when there
   * is no upstream to compare against — which is not zero. */
  readonly ahead: number | null;
  readonly behind: number | null;
}

export interface Commit {
  readonly sha: string;
  readonly short: string;
  readonly subject: string;
  readonly author: string;
  readonly timestampMs: number;
  readonly merge: boolean;
  /** `null` on a branch with no upstream: the row draws no mark rather than
   * claiming a commit is unpushed when nothing can know. */
  readonly pushed: boolean | null;
  readonly added: number;
  readonly deleted: number;
  readonly files: number;
}

/** Which side of the index a diff is taken from. */
export interface DiffRequest {
  readonly path: string;
  readonly staged?: boolean;
  /** A commit's diff, rather than the working tree's. */
  readonly sha?: string;
}

/** The two views the column offers, which are the two views git has. */
export type GitView = "changes" | "history";

/** Everything the fx-host will answer. Every read resolves to a value with
 * its own `error` field; only the write acts reject. */
export interface GitClient {
  branch(): Promise<BranchFacts>;
  status(): Promise<{ entries: readonly StatusEntry[]; error: string | null }>;
  log(): Promise<{ commits: readonly Commit[]; error: string | null }>;
  diff(request: DiffRequest): Promise<{ text: string; error: string | null }>;
  commitFiles(sha: string): Promise<{
    files: readonly { path: string; added: number | null; deleted: number | null }[];
    error: string | null;
  }>;
  stage(paths: readonly string[]): Promise<void>;
  unstage(paths: readonly string[]): Promise<void>;
  discard(paths: readonly string[]): Promise<void>;
}
