// The app's one bridge out of the web world, as the renderer sees it: the
// Electron preload's `window.agentHistoryHost`. Only the app reads it; the
// room sees the `HostPort` this app builds over it (`rosetta-host.ts`).
//
// Everything here is answered by the Electron main process: the daemon's
// state, the home directory, the act of pointing the worktree engine at a
// project, read-only git facts for the root it is showing, document previews,
// and worktree path actions. A browser tab has no bridge at all, which is a
// state the room states rather than papers over.
//
// Actions that leave the app are validated in the main process.
import type {
  InstalledBrowser,
  GitFacts,
  WorktreeFileActionRequest,
  WorktreeFileActionResult,
  WorktreeOpenResult,
} from "@vibefield/rosetta-room";

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
  readonly platform?: string;
  /** Point the worktree engine at a project root. Each accepted request gets
   * a fresh generation, including an explicit retry of the same root. */
  openWorktree(root: string): Promise<WorktreeOpenResult>;
  /** Branch · dirty count · worktrees for the open root, or null when the
   * folder is not a repository. */
  gitFacts(root: string, generation: number): Promise<GitFacts | null>;
  /** Copy absolute path text or reveal one entry in the native file manager. */
  worktreeFileAction(request: WorktreeFileActionRequest): Promise<WorktreeFileActionResult>;
  /**
   * Hand the exact HTML/SVG bytes shown by the viewer to the selected browser.
   *
   * The shell validates the extension and byte limit, writes an app-owned
   * private snapshot, and opens that path. Supplying bytes rather than a path
   * makes this work for transcript resources and prevents a changed source
   * file from becoming a different document between reading and opening.
   */
  openInBrowser(name: string, bytes: Uint8Array, browserId?: string): Promise<boolean>;
  /** Installed browsers. Optional for a preload from an older app version. */
  browsers?(): Promise<readonly InstalledBrowser[]>;
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
