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
  DiscoveriesResponse,
  EnableResponse,
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
import type { DesktopHost } from "./lib/desktop-host";

/** A live subscription the room can end. */
export interface Closable {
  close(): void;
}

/** The index's catalog: who and what exists, the figures a panel draws,
 * search, and the index's own state. Never a transcript body. */
export interface CatalogPort {
  /** The address the catalog reads from, for the room's disconnected state. */
  readonly baseUrl: string;
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
  discoveries(signal?: AbortSignal): Promise<DiscoveriesResponse>;
  enableRuntimes(kinds: string[], signal?: AbortSignal): Promise<EnableResponse>;
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

/** What the room asks a desktop. Absent in a browser tab. */
export type HostPort = DesktopHost;

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
