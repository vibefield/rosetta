// The room's doors. The room dials nothing: every list, figure, transcript
// byte and desktop act it draws arrives through a port a HOST hands it.
// rosetta's own app hands it adapters over its one client; another host
// hands it adapters over its own daemon's doors. The room never knows which.
import type {
  AssetRef,
  CollaborationSubscriptionHandlers,
  CommandResponse,
  ContributionResponse,
  DayUsageResponse,
  HeatmapResponse,
  ImageVariant,
  MetaSubscriptionHandlers,
  RosettaClient,
  SearchRequest,
  SearchResponse,
  SessionSubscriptionHandlers,
  StatusResponse,
  TimelineEntry,
  TimelineMetaPlanSource,
  TimelinePage,
  UsageResponse,
} from "@rosetta/protocol";

/** A live subscription the room can end. */
export interface Closable {
  close(): void;
}

/** The index's catalog: who and what exists, the figures a panel draws,
 * search, and the index's own state. Never a transcript body. */
export interface CatalogPort {
  subscribeMeta(handlers: MetaSubscriptionHandlers): Closable;
  status(signal?: AbortSignal): Promise<StatusResponse>;
  /** Fires when the host's route to the index changes; the room re-asks status. */
  onConnectionChange?(listener: () => void): () => void;
  /** The host's own sentence for why the index cannot answer, or null. */
  unavailable?(): string | null;
  heatmap(
    projectId: string,
    options: { sinceMs?: number; offsetMinutes?: number },
    signal?: AbortSignal,
  ): Promise<HeatmapResponse>;
  contribution(projectId: string, signal?: AbortSignal): Promise<ContributionResponse>;
  usage(
    projectId: string,
    options: { sinceMs?: number; offsetMinutes?: number },
    signal?: AbortSignal,
  ): Promise<UsageResponse>;
  dayUsage(
    projectId: string,
    day: string,
    offsetMinutes: number,
    signal?: AbortSignal,
  ): Promise<DayUsageResponse>;
  search(request: SearchRequest, signal?: AbortSignal): Promise<SearchResponse>;
  boost(sessionId: string, signal?: AbortSignal): Promise<unknown>;
}

/** One session's live transcript, as the room reads it. */
export interface SessionFeed extends Closable {
  entries(): TimelineEntry[];
  fetchPage(cursor: string, limit?: number, signal?: AbortSignal): Promise<TimelinePage>;
  reveal(entries: readonly TimelineEntry[]): void;
  loadEntry(timelineId: string, signal?: AbortSignal): Promise<boolean>;
}

/** One team member's delivered conversation. */
export interface CollaborationFeed extends Closable {
  restart(): void;
}

/**
 * What the room asks of ONE session: its live subscription and the reads that
 * hang off its rows. A `RosettaClient` is one; a host whose daemon wants a
 * per-session credential hands a client that carries it.
 */
export interface SessionClient {
  subscribeSession(sessionId: string, handlers: SessionSubscriptionHandlers): SessionFeed;
  subscribeCollaboration(
    sessionId: string,
    handlers: CollaborationSubscriptionHandlers,
  ): CollaborationFeed;
  timelineEntry(sessionId: string, timelineId: string, signal?: AbortSignal): Promise<TimelineEntry>;
  reportWork(sessionId: string, timelineId: string, signal?: AbortSignal): Promise<TimelineEntry[]>;
  command(sessionId: string, timelineId: string, signal?: AbortSignal): Promise<CommandResponse>;
  readFileTarget(
    sessionId: string,
    timelineId: string,
    targetIndex: number,
    signal?: AbortSignal,
  ): Promise<Uint8Array>;
  readPlan(sessionId: string, source: TimelineMetaPlanSource, signal?: AbortSignal): Promise<Uint8Array>;
  readTextAsset(asset: AssetRef, signal?: AbortSignal): Promise<string>;
  readAsset(asset: AssetRef, signal?: AbortSignal): Promise<Uint8Array>;
}

/**
 * Opens one session's body. The room opens a session at most once at a time
 * and follows every `open` with exactly one `close`, when nothing in the room
 * reads that session any more. `open` may answer at once or with a promise
 * (a host that must ask for a credential first).
 */
export interface SessionPort {
  open(sessionId: string): SessionClient | PromiseLike<SessionClient>;
  close(sessionId: string): void;
}

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

/** An accepted request to start a new worktree engine generation. */
export type WorktreeOpenResult =
  | { readonly ok: true; readonly generation: number }
  | { readonly ok: false; readonly error: string };

export interface WorktreeFileActionRequest {
  readonly root: string;
  readonly generation: number;
  readonly action: "copyPath" | "revealInFileManager";
  /** POSIX paths below the displayed root; empty string names the root. */
  readonly paths: readonly string[];
}

export type WorktreeFileActionResult =
  | { readonly ok: true }
  | { readonly ok: false; readonly error: string };

/** What the host's worktree engine says, in the order it says it: a
 * generation starting or failing, and — once it is up — the two transferred
 * ports for the tree (mille's) and git. A port is a one-owner capability:
 * the room is its only consumer. */
export type WorktreeHostEvent =
  | {
      readonly type: "ports";
      readonly root: string;
      readonly generation: number;
      readonly treePort: MessagePort;
      readonly gitPort: MessagePort;
    }
  | {
      readonly type: "status";
      readonly root: string;
      readonly generation: number;
      readonly state: "starting" | "error";
      readonly message?: string;
    };

/** The worktree guest's engine, which only a desktop host can run. */
export interface WorktreeHostPort {
  /** Point the engine at a project root. Each accepted request starts a fresh
   * generation, including an explicit retry of the same root. */
  open(root: string): Promise<WorktreeOpenResult>;
  /** Branch · dirty count · worktrees for the open root, or null when the
   * folder is not a repository. */
  gitFacts(root: string, generation: number): Promise<GitFacts | null>;
  /** Copy absolute path text or reveal one entry in the native file manager. */
  fileAction(request: WorktreeFileActionRequest): Promise<WorktreeFileActionResult>;
  /** The engine's events. A host replays what arrived before the room
   * listened; the returned function stops listening. */
  ports(listener: (event: WorktreeHostEvent) => void): () => void;
}

/** What the room asks a desktop. A browser tab has none, and the room says
 * so: no worktree guest, a globe for every site icon, no "open in browser". */
export interface HostPort {
  /** The user's home directory, for writing paths as `~/…`; null when unknown. */
  readonly homeDir: string | null;
  /** The desktop's platform (`darwin`, …), for the names of its own acts. */
  readonly platform: string | null;
  /** Hand the exact HTML/SVG bytes the viewer shows to the OS browser. The
   * host validates them and resolves whether it opened anything. */
  openInBrowser(name: string, bytes: Uint8Array): Promise<boolean>;
  /** One site's own icon as an image URL, or null for every honest no — a
   * refused host, no icon, a timeout. The room draws the globe for all. */
  favicon(host: string): Promise<string | null>;
  /** Absent: the worktree guest stays closed and says why. */
  readonly worktree?: WorktreeHostPort | undefined;
}

/** Where every stored image and attachment is addressed. */
export interface AssetSource {
  imageAssetUrl(asset: AssetRef, variant: ImageVariant): string;
  /** The address of one stored attachment member, or null when it names none. */
  attachmentAssetUrl(asset: AssetRef): string | null;
}

/** Everything a host hands the room. */
export interface RoomPorts {
  readonly catalog: CatalogPort;
  readonly sessions: SessionPort;
  readonly assets: AssetSource;
  /** Absent: a browser tab, which the room states rather than papers over. */
  readonly host?: HostPort | null;
}

// A RosettaClient is a SessionClient as it stands; this line fails to compile
// the day the two drift apart.
type RosettaClientIsASessionClient = RosettaClient extends SessionClient ? true : never;
const rosettaClientIsASessionClient: RosettaClientIsASessionClient = true;
void rosettaClientIsASessionClient;
