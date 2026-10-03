// What column ③ adds around mille's own types. The git shapes belong to the
// host's worktree port that answers them — see `HostPort` in `ports.ts`.

import type { PortFileExplorer } from "@vibecook/mille/port";

/** The three views this column offers of one project: the worktree as it
 * stands, what git has not recorded yet, and what it has. */
export type RepoTab = "files" | "changes" | "history";

/** A live connection to one mille engine, and the root it is walking.
 *
 * The root travels WITH the connection rather than beside it: the engine
 * follows a project switch with a lag (kill, re-fork, walk), and the column
 * must be able to name what the tree is actually showing at any moment, not
 * what was last asked for. */
export interface WorktreeEngine {
  readonly fx: PortFileExplorer;
  readonly root: string;
  /** Utility-process generation. Entry ids are scoped to this identity. */
  readonly generation: number;
  dispose(): void;
}
