// What column ③ adds around mille's own types. The git shapes belong to the
// shell bridge that answers them — see `lib/desktop-host`.

import type { PortFileExplorer } from "@vibecook/mille/port";

/** A live connection to one mille engine, and the root it is walking.
 *
 * The root travels WITH the connection rather than beside it: the engine
 * follows a project switch with a lag (kill, re-fork, walk), and the column
 * must be able to name what the tree is actually showing at any moment, not
 * what was last asked for. */
export interface WorktreeEngine {
  readonly fx: PortFileExplorer;
  readonly root: string;
  dispose(): void;
}
