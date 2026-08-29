// The room's one bridge out of the web world, as the renderer sees it.
//
// Everything here is answered by the Electron main process: the home
// directory, the act of pointing the worktree engine at a project, read-only
// git facts for the root it is showing, and handing one HTML file to the
// browser. A browser tab has no bridge at all, which is a state the room
// states rather than papers over.
//
// Nothing here writes to disk, and the one act that leaves the app is
// validated on the far side rather than here — see `openInBrowser`.

/** One checkout of the repository a root belongs to. */
export interface GitWorktree {
  readonly path: string;
  /** Absent on a detached HEAD. */
  readonly branch: string | null;
}

/** Read-only git facts for one root. Every field is nullable because a folder
 * without a repository is a fact, not a failure: the room says less rather
 * than something untrue. */
export interface GitFacts {
  readonly branch: string | null;
  readonly dirty: number | null;
  readonly worktrees: readonly GitWorktree[];
}

export interface DesktopHost {
  /** The user's home directory, for writing paths as `~/…`. */
  readonly homeDir?: string;
  /** Point the worktree engine at a project root. Resolves false if the
   * path is not a directory the shell will open. */
  openWorktree(root: string): Promise<boolean>;
  /** Branch · dirty count · worktrees for the open root, or null when the
   * folder is not a repository. */
  gitFacts(root: string): Promise<GitFacts | null>;
  /**
   * Hand a file in the open worktree to the OS — for the one file kind this
   * room will not render itself, HTML.
   *
   * The renderer ASKS; the shell decides. It re-derives the extension and
   * re-checks the path against the root it forked the engine for, because a
   * renderer showing untrusted repository content is exactly the process
   * whose word on "which file should the system open" is worth least.
   * Resolves false when it refuses, which the room reports rather than
   * swallowing.
   */
  openInBrowser(path: string): Promise<boolean>;
}

interface HostWindow {
  agentHistoryHost?: DesktopHost;
}

/** The desktop shell's bridge, or `null` when the room runs in a plain
 * browser tab. Callers branch on it — there is no stub that pretends. */
export function desktopHost(): DesktopHost | null {
  return (window as HostWindow).agentHistoryHost ?? null;
}
