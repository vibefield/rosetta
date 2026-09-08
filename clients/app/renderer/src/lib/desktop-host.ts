// The room's one bridge out of the web world, as the renderer sees it.
//
// Everything here is answered by the Electron main process: the home
// directory, the act of pointing the worktree engine at a project, read-only
// git facts for the root it is showing, and handing one viewed document to the
// browser. A browser tab has no bridge at all, which is a state the room
// states rather than papers over.
//
// Nothing here chooses a destination or writes directly, and the one act that
// leaves the app is validated on the far side — see `openInBrowser`.

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

export interface DesktopDaemonState {
  readonly phase: "starting" | "ready" | "restarting" | "failed" | "stopped" | "external";
  readonly origin: string | null;
  readonly error: string | null;
  readonly revision: number;
}

export interface DesktopHost {
  daemonState?(): Promise<DesktopDaemonState>;
  onDaemonState?(listener: (state: DesktopDaemonState) => void): () => void;
  /** The user's home directory, for writing paths as `~/…`. */
  readonly homeDir?: string;
  /** Point the worktree engine at a project root. Each accepted request gets
   * a fresh generation, including an explicit retry of the same root. */
  openWorktree(root: string): Promise<WorktreeOpenResult>;
  /** Branch · dirty count · worktrees for the open root, or null when the
   * folder is not a repository. */
  gitFacts(root: string, generation: number): Promise<GitFacts | null>;
  /**
   * Hand the exact HTML/SVG bytes shown by the viewer to the OS browser.
   *
   * The shell validates the extension and byte limit, writes an app-owned
   * private snapshot, and opens that path. Supplying bytes rather than a path
   * makes this work for transcript resources and prevents a changed source
   * file from becoming a different document between reading and opening.
   */
  openInBrowser(name: string, bytes: Uint8Array): Promise<boolean>;
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
