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

/** An accepted request to start a new worktree utility-process generation. */
export type WorktreeOpenResult =
  | { readonly ok: true; readonly generation: number }
  | { readonly ok: false; readonly error: string };

export interface DesktopHost {
  /** The user's home directory, for writing paths as `~/…`. */
  readonly homeDir?: string;
  /** Point the worktree engine at a project root. Each accepted request gets
   * a fresh generation, including an explicit retry of the same root. */
  openWorktree(root: string): Promise<WorktreeOpenResult>;
  /** Branch · dirty count · worktrees for the open root, or null when the
   * folder is not a repository. */
  gitFacts(root: string, generation: number): Promise<GitFacts | null>;
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
  /**
   * One site's own icon, for a web line in the transcript.
   *
   * The same bargain as `openInBrowser`: the renderer ASKS with a host it
   * read out of somebody's transcript, and the shell decides whether that
   * host is dialled at all. It fetches `https://<host>/favicon.ico` from the
   * SITE — never Google's or DuckDuckGo's favicon service, which would be
   * handed the user's whole agentic past one host at a time — caches the
   * answer per host, and resolves a `data:` URL.
   *
   * Resolves null for every honest no: a host it refuses, a site with no
   * icon, a 404, a timeout, a body that was not an image. The room draws the
   * globe for all of them, because to a reader they are one fact.
   */
  favicon(host: string): Promise<string | null>;
}

interface HostWindow {
  agentHistoryHost?: DesktopHost;
}

/** The desktop shell's bridge, or `null` when the room runs in a plain
 * browser tab. Callers branch on it — there is no stub that pretends. */
export function desktopHost(): DesktopHost | null {
  return (window as HostWindow).agentHistoryHost ?? null;
}
