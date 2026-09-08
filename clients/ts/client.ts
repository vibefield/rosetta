// Hand-written client over the generated protocol types. This is the ONE
// place the subscribe state machine lives outside Rust: snapshot at a
// stamp, apply mutations keyed by timeline_id (including patches into
// earlier rows), obey Resync barriers, and reconnect with a resume stamp.
// Consumers render; they never re-implement protocol semantics.

import { OrderedCache } from "./ordered-cache.ts";
import { PROTOCOL_VERSION } from "./protocol-version.mjs";
export { PROTOCOL_VERSION } from "./protocol-version.mjs";
import type { TimelineEntryResponse } from "./generated/TimelineEntryResponse";
import type { ResponseRuntime } from "./generated/ResponseRuntime";
import type { ApiError } from "./generated/ApiError";
import type { AssetRef } from "./generated/AssetRef";
import type { BoostResponse } from "./generated/BoostResponse";
import type { ContributionResponse } from "./generated/ContributionResponse";
import type { CommandResponse } from "./generated/CommandResponse";
import type { CollaborationTimeline } from "./generated/CollaborationTimeline";
import type { DiscoveriesResponse } from "./generated/DiscoveriesResponse";
import type { EnableResponse } from "./generated/EnableResponse";
import type { ErrorCode } from "./generated/ErrorCode";
import type { HeatmapResponse } from "./generated/HeatmapResponse";
import type { Mutation } from "./generated/Mutation";
import type { ProjectRow } from "./generated/ProjectRow";
import type { ProjectsResponse } from "./generated/ProjectsResponse";
import type { RuntimeRow } from "./generated/RuntimeRow";
import type { RuntimesResponse } from "./generated/RuntimesResponse";
import type { SearchRequest } from "./generated/SearchRequest";
import type { SearchResponse } from "./generated/SearchResponse";
import type { ServerFrame } from "./generated/ServerFrame";
import type { SessionResponse } from "./generated/SessionResponse";
import type { SessionRow } from "./generated/SessionRow";
import type { SessionsResponse } from "./generated/SessionsResponse";
import type { Stamp } from "./generated/Stamp";
import type { StatusResponse } from "./generated/StatusResponse";
import type { TimelinePage } from "./generated/TimelinePage";
import type { TimelineEntry } from "./generated/TimelineEntry";
import type { TimelineMeta } from "./generated/TimelineMeta";
import type { TimelineMetaPlanSource } from "./generated/TimelineMetaPlanSource";
import type { ToolInteraction } from "./generated/ToolInteraction";
import type { UsageResponse } from "./generated/UsageResponse";

export const DEFAULT_BASE_URL = "http://127.0.0.1:4640";

export interface RosettaClientOptions {
  baseUrl?: string;
  /** Injectable for Node or test environments. */
  fetchFn?: typeof fetch;
  WebSocketImpl?: typeof WebSocket;
  requestTimeoutMs?: number;
  notificationIntervalMs?: number;
}

export class ApiRequestError extends Error {
  readonly status: number;
  readonly code: ErrorCode | null;

  constructor(status: number, code: ErrorCode | null, message: string) {
    super(message);
    this.name = "ApiRequestError";
    this.status = status;
    this.code = code;
  }
}

/** The bound watermark no longer matches: refetch from the head. */
export class CursorExpiredError extends ApiRequestError {
  constructor(message: string) {
    super(410, "cursor_expired", message);
    this.name = "CursorExpiredError";
  }
}

export type SubscriptionStatus = "connecting" | "syncing" | "live" | "resyncing" | "closed";

export interface SessionSubscriptionHandlers {
  /** Every replacement invalidates all loaded history, anchors, and in-flight reads. */
  onReset?: () => void;
  onSnapshot?: (page: TimelinePage) => void;
  /** The full ordered visible timeline after each coalesced change. */
  onChange?: (entries: TimelineEntry[]) => void;
  /** The complete light work index. Bodies remain behind lazy references. */
  onTimelineMetaChange?: (timelineMeta: TimelineMeta) => void;
  onStatus?: (status: SubscriptionStatus) => void;
  onError?: (error: ApiError) => void;
}

export interface MetaState {
  runtimes: RuntimeRow[];
  projects: ProjectRow[];
  sessions: SessionRow[];
}

export interface MetaSubscriptionHandlers {
  onChange?: (state: MetaState) => void;
  onStatus?: (status: SubscriptionStatus) => void;
  onError?: (error: ApiError) => void;
}

interface QueryParams {
  [key: string]: string | number | boolean | null | undefined;
}

function queryString(params: QueryParams): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== null && value !== undefined) {
      search.set(key, String(value));
    }
  }
  const text = search.toString();
  return text === "" ? "" : `?${text}`;
}

const RECONNECT_BASE_MS = 500;
const RECONNECT_MAX_MS = 10_000;

/** Keep additive timeline-meta fields safe across a daemon/client rolling
 * restart. The protocol handshake still owns incompatible changes; a missing
 * newly-added collection is the one backwards-compatible case we accept. */
function normalizeTimelineMeta(meta: TimelineMeta): TimelineMeta {
  return {
    tasks: meta.tasks ?? [],
    workflows: meta.workflows ?? [],
    plans: meta.plans ?? [],
    artifacts: meta.artifacts ?? [],
  };
}

/** `run` is a query-time join and therefore absent from row-level mutations.
 * Keep it only while both canonical actions still identify the same run. A
 * replacement snapshot starts from an empty map and remains authoritative. */
function retainHydratedWorkflowRun(
  current: ToolInteraction,
  incoming: ToolInteraction,
): ToolInteraction {
  const before = current.action;
  const after = incoming.action;
  return before.type === "workflow" &&
    after.type === "workflow" &&
    before.run !== null &&
    after.run === null &&
    before.run_id !== null &&
    after.run_id === before.run_id
    ? { ...incoming, action: { ...after, run: before.run } }
    : incoming;
}

export class RosettaClient {
  private origin: string;
  get baseUrl(): string {
    return this.origin;
  }
  private connection = new AbortController();
  private readonly connectionListeners = new Set<() => void>();
  private readonly subscriptions = new Set<BaseSubscription>();
  private readonly requestTimeoutMs: number;
  private readonly notificationIntervalMs: number;
  private readonly fetchFn: typeof fetch;
  private readonly WebSocketImpl: typeof WebSocket;

  constructor(options: RosettaClientOptions = {}) {
    this.origin = (options.baseUrl ?? DEFAULT_BASE_URL).replace(/\/$/, "");
    this.fetchFn = options.fetchFn ?? fetch.bind(globalThis);
    this.WebSocketImpl = options.WebSocketImpl ?? WebSocket;
    this.requestTimeoutMs = options.requestTimeoutMs ?? 30_000;
    this.notificationIntervalMs = options.notificationIntervalMs ?? 16;
  }

  private async response(path: string, init?: RequestInit): Promise<Response> {
    const signal = AbortSignal.any([
      AbortSignal.timeout(this.requestTimeoutMs),
      this.connection.signal,
      ...(init?.signal ? [init.signal] : []),
    ]);
    const response = await this.fetchFn(`${this.baseUrl}${path}`, {
      ...init,
      signal,
      redirect: "error",
    });
    signal.throwIfAborted();
    const version = response.headers.get("x-rosetta-protocol-version");
    if (version !== String(PROTOCOL_VERSION)) {
      throw new ApiRequestError(
        response.status,
        "incompatible_protocol",
        `Daemon protocol ${version ?? "unknown"} is incompatible with this app (expected ${PROTOCOL_VERSION}). Restart with the matching daemon.`,
      );
    }
    if (!response.ok) {
      let code: ErrorCode | null = null;
      let message = `${response.status} on ${path}`;
      try {
        const body = (await response.json()) as { error?: ApiError };
        if (body.error) {
          code = body.error.code;
          message = body.error.message;
        }
      } catch {
        // Non-JSON error body; keep the status-line message.
      }
      if (code === "cursor_expired") {
        throw new CursorExpiredError(message);
      }
      throw new ApiRequestError(response.status, code, message);
    }
    return response;
  }

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    return (await (await this.response(path, init)).json()) as T;
  }

  status(signal?: AbortSignal): Promise<StatusResponse> {
    return this.request("/api/status", { signal });
  }

  /** The roll call's census — what discovery found, content-free. */
  discoveries(signal?: AbortSignal): Promise<DiscoveriesResponse> {
    return this.request("/api/discoveries", { signal });
  }

  /** Consent: enroll the chosen agent kinds. Grantable exactly once, while
   * the daemon is awaiting consent. */
  enableRuntimes(kinds: string[], signal?: AbortSignal): Promise<EnableResponse> {
    return this.request("/api/runtimes/enable", {
      method: "POST",
      signal,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ kinds }),
    });
  }

  /** Attention-directed ingestion: the reader opened a still-queued
   * session — ask the daemon to read it next. Advisory; safe to fire on
   * every selection of a pending session. */
  boost(sessionId: string, signal?: AbortSignal): Promise<BoostResponse> {
    return this.request("/api/ingest/boost", {
      method: "POST",
      signal,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ session_id: sessionId }),
    });
  }

  runtimes(signal?: AbortSignal): Promise<RuntimesResponse> {
    return this.request("/api/runtimes", { signal });
  }

  projects(signal?: AbortSignal): Promise<ProjectsResponse> {
    return this.request("/api/projects", { signal });
  }

  sessions(
    options: { project?: string; limit?: number } = {},
    signal?: AbortSignal,
  ): Promise<SessionsResponse> {
    return this.request(`/api/sessions${queryString(options)}`, { signal });
  }

  session(id: string, signal?: AbortSignal): Promise<SessionResponse> {
    return this.request(`/api/sessions/${encodeURIComponent(id)}`, { signal });
  }

  /**
   * One backward page. Pass the previous page's `cursor` to continue; a
   * rewrite underneath throws {@link CursorExpiredError} — refetch from the
   * head (no cursor).
   */
  timeline(
    sessionId: string,
    options: { limit?: number; cursor?: string } = {},
    signal?: AbortSignal,
  ): Promise<TimelinePage> {
    return this.request(
      `/api/sessions/${encodeURIComponent(sessionId)}/timeline${queryString(options)}`,
      { signal },
    );
  }

  /** Complete delivered collaboration history for one team member. The
   * response deliberately excludes private prose and tool rows. */
  collaborationTimeline(sessionId: string, signal?: AbortSignal): Promise<CollaborationTimeline> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/collaboration`, { signal });
  }

  /** An exact row and the generation it belongs to. */
  timelineEntryResponse(
    sessionId: string,
    timelineId: string,
    signal?: AbortSignal,
  ): Promise<TimelineEntryResponse> {
    return this.request(
      `/api/sessions/${encodeURIComponent(sessionId)}/timeline/${encodeURIComponent(timelineId)}`,
      { signal },
    );
  }

  async timelineEntry(
    sessionId: string,
    timelineId: string,
    signal?: AbortSignal,
  ): Promise<TimelineEntry> {
    return (await this.timelineEntryResponse(sessionId, timelineId, signal)).entry;
  }

  reportWork(
    sessionId: string,
    timelineId: string,
    signal?: AbortSignal,
  ): Promise<TimelineEntry[]> {
    return this.request(
      `/api/sessions/${encodeURIComponent(sessionId)}/timeline/${encodeURIComponent(timelineId)}/work`,
      { signal },
    );
  }

  /** Recover one deliberately omitted command body from its raw-ledger
   * record. Nothing is fetched until the reader opens the disclosure. */
  command(sessionId: string, timelineId: string, signal?: AbortSignal): Promise<CommandResponse> {
    return this.request(
      `/api/sessions/${encodeURIComponent(sessionId)}/timeline/${encodeURIComponent(timelineId)}/command`,
      { signal },
    );
  }

  /** Read the live file named by one canonical file-bearing target. The caller
   * supplies identities and an index, never a path; the daemon resolves the
   * exact path already stored on that timeline row. */
  async readFileTarget(
    sessionId: string,
    timelineId: string,
    targetIndex: number,
    signal?: AbortSignal,
  ): Promise<Uint8Array> {
    const response = await this.response(
      `/api/sessions/${encodeURIComponent(sessionId)}/timeline/${encodeURIComponent(timelineId)}/read/${targetIndex}`,
      { signal },
    );
    return new Uint8Array(await response.arrayBuffer());
  }

  /** Resolve a canonical oversized-text member only when its disclosure is
   * opened. Asset references are daemon-owned and may not redirect this
   * authenticated/local client to another origin. */
  async readTextAsset(asset: AssetRef, signal?: AbortSignal): Promise<string> {
    const base = new URL(`${this.baseUrl}/`);
    const url = new URL(asset.url, base);
    if ((url.protocol !== "http:" && url.protocol !== "https:") || url.origin !== base.origin) {
      throw new Error("the stored text has an unsupported address");
    }
    return (await this.response(`${url.pathname}${url.search}`, { signal })).text();
  }

  async readAsset(asset: AssetRef, signal?: AbortSignal): Promise<Uint8Array> {
    const base = new URL(`${this.baseUrl}/`);
    const url = new URL(asset.url, base);
    if (url.origin !== base.origin || !["http:", "https:"].includes(url.protocol)) {
      throw new Error("the stored asset has an unsupported address");
    }
    return new Uint8Array(
      await (await this.response(`${url.pathname}${url.search}`, { signal })).arrayBuffer(),
    );
  }

  /** Read one exact plan document selected from timeline_meta. */
  async readPlan(
    sessionId: string,
    source: TimelineMetaPlanSource,
    signal?: AbortSignal,
  ): Promise<Uint8Array> {
    const path =
      source.type === "revision"
        ? `/api/sessions/${encodeURIComponent(sessionId)}/plans/revisions/${source.revision}`
        : `/api/sessions/${encodeURIComponent(sessionId)}/timeline/${encodeURIComponent(source.timeline_id)}/plan`;
    const response = await this.response(path, { signal });
    return new Uint8Array(await response.arrayBuffer());
  }

  search(request: SearchRequest, signal?: AbortSignal): Promise<SearchResponse> {
    return this.request(
      `/api/search${queryString({
        q: request.q,
        limit: request.limit,
        session: request.session,
        runtime: request.runtime,
        agent: request.agent,
        project: request.project,
        kind: request.kind,
        is_error: request.is_error,
        after_ts: request.after_ts,
        before_ts: request.before_ts,
      })}`,
      { signal },
    );
  }

  heatmap(
    projectId: string,
    options: { sinceMs?: number; offsetMinutes?: number } = {},
    signal?: AbortSignal,
  ): Promise<HeatmapResponse> {
    return this.request(
      `/api/projects/${encodeURIComponent(projectId)}/heatmap${queryString({
        since_ms: options.sinceMs,
        offset_minutes: options.offsetMinutes,
      })}`,
      { signal },
    );
  }

  contribution(projectId: string, signal?: AbortSignal): Promise<ContributionResponse> {
    return this.request(`/api/projects/${encodeURIComponent(projectId)}/contribution`, { signal });
  }

  /** The heatmap figure's decomposition over the same window: per-model days
   * (they sum back to the heatmap exactly) plus an hour-of-day profile. */
  usage(
    projectId: string,
    options: { sinceMs?: number; offsetMinutes?: number } = {},
    signal?: AbortSignal,
  ): Promise<UsageResponse> {
    return this.request(
      `/api/projects/${encodeURIComponent(projectId)}/usage${queryString({
        since_ms: options.sinceMs,
        offset_minutes: options.offsetMinutes,
      })}`,
      { signal },
    );
  }

  /** A restarted app-owned daemon may bind another port. Abort old requests
   * and reconnect existing consumers without losing the reader's selection. */
  setBaseUrl(baseUrl: string): void {
    const origin = baseUrl.replace(/\/$/, "");
    if (origin === this.origin) return;
    this.origin = origin;
    this.connection.abort();
    this.connection = new AbortController();
    for (const subscription of this.subscriptions) subscription.restart();
    for (const listener of this.connectionListeners) listener();
  }

  onConnectionChange(listener: () => void): () => void {
    this.connectionListeners.add(listener);
    return () => {
      this.connectionListeners.delete(listener);
    };
  }

  private transport(path: string): SubscriptionTransport {
    return {
      url: () => `${this.baseUrl.replace(/^http/, "ws")}${path}`,
      socket: this.WebSocketImpl,
      interval: this.notificationIntervalMs,
      onClose: (sub) => this.subscriptions.delete(sub),
    };
  }

  subscribeSession(sessionId: string, handlers: SessionSubscriptionHandlers): SessionSubscription {
    const sub = new SessionSubscription(
      this.transport(`/ws/sessions/${encodeURIComponent(sessionId)}`),
      handlers,
      this,
      sessionId,
    );
    this.subscriptions.add(sub);
    return sub;
  }

  subscribeMeta(handlers: MetaSubscriptionHandlers): MetaSubscription {
    const sub = new MetaSubscription(this.transport("/ws/meta"), handlers);
    this.subscriptions.add(sub);
    return sub;
  }

  subscribeCollaboration(
    sessionId: string,
    handlers: CollaborationSubscriptionHandlers,
  ): CollaborationSubscription {
    const sub = new CollaborationSubscription(
      this.transport(`/ws/collaboration/${encodeURIComponent(sessionId)}`),
      handlers,
    );
    this.subscriptions.add(sub);
    return sub;
  }
}

interface SubscriptionTransport {
  url: () => string;
  socket: typeof WebSocket;
  interval: number;
  onClose: (sub: BaseSubscription) => void;
}

abstract class BaseSubscription {
  protected stamp: Stamp | null = null;
  protected closed = false;
  private readonly transport: SubscriptionTransport;
  private socket: WebSocket | null = null;
  private attempts = 0;
  private status: SubscriptionStatus | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private deadline: ReturnType<typeof setTimeout> | null = null;
  private changeTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(transport: SubscriptionTransport) {
    this.transport = transport;
  }
  protected start(): void {
    this.connect();
  }

  private connect(): void {
    if (this.closed) return;
    this.reconnectTimer = null;
    const resume = this.stamp === null ? "" : `?epoch=${this.stamp.epoch}&seq=${this.stamp.seq}`;
    this.setStatus("connecting");
    let socket: WebSocket;
    try {
      socket = new this.transport.socket(`${this.transport.url()}${resume}`);
    } catch {
      this.retry();
      return;
    }
    this.socket = socket;
    let greeted = false;
    // Opening TCP is not successful synchronization. Keep backoff until live.
    this.deadline = setTimeout(() => socket.close(), 15_000);
    socket.onopen = () => {
      if (this.socket === socket && !this.closed) this.setStatus("syncing");
    };
    socket.onmessage = (event: MessageEvent) => {
      if (this.socket !== socket || this.closed) return;
      try {
        const frame = JSON.parse(String(event.data)) as ServerFrame;
        if (frame.frame === "hello") {
          if (frame.protocol_version !== PROTOCOL_VERSION) {
            this.onError({
              code: "incompatible_protocol",
              message: `Daemon protocol ${frame.protocol_version} is incompatible with this app (expected ${PROTOCOL_VERSION}). Restart with the matching daemon.`,
            });
            this.close();
            return;
          }
          greeted = true;
          // Hello advertises a target. It never acknowledges data we received.
          return;
        }
        if (!greeted) throw new Error("The daemon sent data before its protocol handshake.");
        if (frame.frame === "error") this.onError(frame.error);
        else this.handleFrame(frame);
      } catch (error) {
        this.onError({
          code: "internal",
          message: error instanceof Error ? error.message : "Invalid daemon frame",
        });
        this.stamp = null;
        this.invalidate();
        socket.close();
      }
    };
    socket.onclose = () => {
      if (this.socket !== socket) return;
      this.socket = null;
      this.clearDeadline();
      if (!this.closed) this.retry();
    };
    socket.onerror = () => {
      /* close owns reconnect */
    };
  }

  private retry(): void {
    if (this.closed) return;
    this.setStatus("connecting");
    const delay = Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** this.attempts++);
    this.reconnectTimer = setTimeout(() => this.connect(), delay);
  }

  private clearDeadline(): void {
    if (this.deadline !== null) clearTimeout(this.deadline);
    this.deadline = null;
  }

  protected live(): void {
    this.attempts = 0;
    this.clearDeadline();
    this.setStatus("live");
  }

  protected setStatus(status: SubscriptionStatus): void {
    if (this.status === status) return;
    this.status = status;
    if (status === "resyncing" && this.socket !== null) {
      this.clearDeadline();
      const socket = this.socket;
      this.deadline = setTimeout(() => socket.close(), 15_000);
    }
    this.onStatus(status);
  }

  protected changed(immediate = false): void {
    if (this.closed) return;
    if (immediate || this.transport.interval === 0) {
      this.cancelChange();
      this.flushChanges();
    } else if (this.changeTimer === null) {
      this.changeTimer = setTimeout(() => {
        this.changeTimer = null;
        if (!this.closed) this.flushChanges();
      }, this.transport.interval);
    }
  }

  protected cancelChange(): void {
    if (this.changeTimer !== null) clearTimeout(this.changeTimer);
    this.changeTimer = null;
  }

  /** Drop a generation and request an authoritative snapshot. */
  restart(): void {
    if (this.closed) return;
    this.stamp = null;
    this.invalidate();
    const socket = this.socket;
    this.socket = null;
    this.clearDeadline();
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    socket?.close();
    this.connect();
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.clearDeadline();
    this.cancelChange();
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    const socket = this.socket;
    this.socket = null;
    socket?.close();
    this.dispose();
    this.setStatus("closed");
    this.transport.onClose(this);
  }

  protected abstract invalidate(): void;
  protected abstract handleFrame(frame: ServerFrame): void;
  protected abstract flushChanges(): void;
  protected abstract onStatus(status: SubscriptionStatus): void;
  protected abstract onError(error: ApiError): void;
  protected dispose(): void {}
}

const timelineOrder = (a: TimelineEntry, b: TimelineEntry) =>
  a.ordinal - b.ordinal || a.timeline_id.localeCompare(b.timeline_id);

/** Apply the same canonical patch to visible rows, held pages, and anchors. */
function patchRow(row: TimelineEntry | undefined, mutation: Mutation): TimelineEntry | undefined {
  switch (mutation.type) {
    case "message": {
      const { type: _tag, ...incoming } = mutation;
      return row?.body.type === "tool_use" && incoming.body.type === "tool_use"
        ? {
            ...incoming,
            body: {
              ...incoming.body,
              tool: retainHydratedWorkflowRun(row.body.tool, incoming.body.tool),
            },
          }
        : incoming;
    }
    case "tool_updated":
      return row?.body.type === "tool_use"
        ? {
            ...row,
            body: { ...row.body, tool: retainHydratedWorkflowRun(row.body.tool, mutation.tool) },
          }
        : row;
    case "usage":
      return row ? { ...row, usage: mutation.tokens, model: mutation.model, at: mutation.ts } : row;
    default:
      return row;
  }
}

interface PendingRead {
  controller: AbortController;
  mutations: Mutation[];
}

export class SessionSubscription extends BaseSubscription {
  private readonly handlers: SessionSubscriptionHandlers;
  private readonly client: RosettaClient;
  private readonly sessionId: string;
  private readonly rows = new Map<string, TimelineEntry>();
  private readonly visible = new OrderedCache<TimelineEntry>(
    (row) => row.timeline_id,
    timelineOrder,
  );
  private timelineMeta: TimelineMeta = { tasks: [], workflows: [], plans: [], artifacts: [] };
  private generation = 0;
  private watermark: string | null = null;
  private readonly pending = new Set<PendingRead>();
  private rowsDirty = false;
  private metaDirty = false;

  constructor(
    transport: SubscriptionTransport,
    handlers: SessionSubscriptionHandlers,
    client: RosettaClient,
    sessionId: string,
  ) {
    super(transport);
    this.handlers = handlers;
    this.client = client;
    this.sessionId = sessionId;
    this.start();
  }

  entries(): TimelineEntry[] {
    return this.visible.values();
  }

  protected dispose(): void {
    for (const read of this.pending) read.controller.abort();
    this.pending.clear();
  }

  protected invalidate(): void {
    const hadSnapshot = this.watermark !== null;
    this.cancelChange();
    this.generation++;
    this.watermark = null;
    this.dispose();
    this.rows.clear();
    this.visible.replace([]);
    this.timelineMeta = { tasks: [], workflows: [], plans: [], artifacts: [] };
    this.rowsDirty = this.metaDirty = false;
    if (hadSnapshot) this.handlers.onReset?.();
  }

  protected handleFrame(frame: ServerFrame): void {
    switch (frame.frame) {
      case "timeline_snapshot":
        this.invalidate();
        this.watermark = frame.watermark;
        for (const row of frame.entries) this.rows.set(row.timeline_id, row);
        this.visible.replace(frame.entries);
        this.timelineMeta = normalizeTimelineMeta(frame.timeline_meta);
        this.stamp = frame.stamp;
        this.handlers.onSnapshot?.({
          entries: frame.entries,
          cursor: frame.cursor,
          watermark: frame.watermark,
        });
        this.rowsDirty = this.metaDirty = true;
        this.live();
        this.changed(true);
        break;
      case "caught_up":
        // A partial replay leaves stamp unchanged and is replayed again.
        this.stamp = frame.stamp;
        this.live();
        this.changed(true);
        break;
      case "timeline_meta_snapshot":
        this.timelineMeta = normalizeTimelineMeta(frame.timeline_meta);
        this.metaDirty = true;
        if (frame.stamp !== null) {
          this.stamp = frame.stamp;
          this.live();
        }
        this.changed();
        break;
      case "mutation":
        if (frame.mutation.type === "session_swept") {
          this.restart();
          return;
        }
        this.apply(frame.mutation);
        if (frame.stamp !== null) {
          this.stamp = frame.stamp;
          this.live();
        }
        this.changed();
        break;
      case "resync":
        this.stamp = null;
        this.invalidate();
        this.setStatus("resyncing");
        break;
      default:
        break;
    }
  }

  private apply(mutation: Mutation): void {
    if (
      mutation.type !== "message" &&
      mutation.type !== "tool_updated" &&
      mutation.type !== "usage"
    )
      return;
    for (const read of this.pending) {
      // An overwhelmed read retries instead of silently dropping a late patch.
      if (read.mutations.length >= 4096) read.controller.abort();
      else read.mutations.push(mutation);
    }
    const current = this.rows.get(mutation.timeline_id);
    const row = patchRow(current, mutation);
    if (row === undefined || row === current) return;
    this.rows.set(row.timeline_id, row);
    if (
      this.visible.get(row.timeline_id) !== undefined ||
      (current === undefined && mutation.type === "message")
    ) {
      this.visible.set(row);
      this.rowsDirty = true;
    }
  }

  /** Hold a page in this cache without displaying it until the reader pulls. */
  async fetchPage(cursor: string, limit = 200, signal?: AbortSignal): Promise<TimelinePage> {
    return this.read(
      async (signal) => this.client.timeline(this.sessionId, { cursor, limit }, signal),
      false,
      signal,
    );
  }

  /** Reveal cached identities using their latest patched values. */
  reveal(entries: readonly TimelineEntry[]): void {
    for (const entry of entries) {
      const row = this.rows.get(entry.timeline_id);
      if (row !== undefined) this.visible.set(row);
    }
    this.rowsDirty = true;
    this.changed(true);
  }

  async loadEntry(timelineId: string, signal?: AbortSignal): Promise<boolean> {
    await this.read(
      async (signal) => {
        const result = await this.client.timelineEntryResponse(this.sessionId, timelineId, signal);
        return { entries: [result.entry], watermark: result.watermark, cursor: null };
      },
      true,
      signal,
    );
    return true;
  }

  private async read(
    load: (signal: AbortSignal) => Promise<TimelinePage>,
    show: boolean,
    signal?: AbortSignal,
  ): Promise<TimelinePage> {
    if (this.closed || this.watermark === null)
      throw new DOMException("The session is synchronizing", "AbortError");
    const generation = this.generation;
    const read: PendingRead = { controller: new AbortController(), mutations: [] };
    this.pending.add(read);
    const combined = AbortSignal.any([read.controller.signal, ...(signal ? [signal] : [])]);
    try {
      const page = await load(combined);
      combined.throwIfAborted();
      if (generation !== this.generation || this.closed)
        throw new DOMException("The session changed", "AbortError");
      if (page.watermark !== this.watermark)
        throw new CursorExpiredError("The session was rewritten");
      for (const incoming of page.entries) {
        let row: TimelineEntry | undefined = incoming;
        for (const mutation of read.mutations) {
          if (
            (mutation.type === "message" ||
              mutation.type === "tool_updated" ||
              mutation.type === "usage") &&
            mutation.timeline_id === incoming.timeline_id
          )
            row = patchRow(row, mutation);
        }
        if (row === undefined) continue;
        this.rows.set(row.timeline_id, row);
        if (show || this.visible.get(row.timeline_id) !== undefined) {
          this.visible.set(row);
          this.rowsDirty = true;
        }
      }
      this.changed(show);
      return page;
    } catch (error) {
      if (error instanceof CursorExpiredError && generation === this.generation) {
        this.restart();
        this.handlers.onError?.({ code: "cursor_expired", message: error.message });
      }
      throw error;
    } finally {
      this.pending.delete(read);
    }
  }

  protected flushChanges(): void {
    if (this.rowsDirty) {
      this.rowsDirty = false;
      this.handlers.onChange?.(this.entries());
    }
    if (this.metaDirty) {
      this.metaDirty = false;
      this.handlers.onTimelineMetaChange?.(this.timelineMeta);
    }
  }
  protected onStatus(status: SubscriptionStatus): void {
    this.handlers.onStatus?.(status);
  }
  protected onError(error: ApiError): void {
    this.handlers.onError?.(error);
  }
}

export class MetaSubscription extends BaseSubscription {
  private readonly handlers: MetaSubscriptionHandlers;
  private readonly runtimes = new OrderedCache<RuntimeRow>(
    (r) => r.id,
    (a, b) => a.id.localeCompare(b.id),
  );
  private readonly projects = new OrderedCache<ProjectRow>(
    (r) => r.id,
    (a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id),
  );
  private readonly sessions = new OrderedCache<SessionRow>(
    (r) => r.id,
    (a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id),
  );
  private dirty = false;

  constructor(transport: SubscriptionTransport, handlers: MetaSubscriptionHandlers) {
    super(transport);
    this.handlers = handlers;
    this.start();
  }
  state(): MetaState {
    return {
      runtimes: this.runtimes.values(),
      projects: this.projects.values(),
      sessions: this.sessions.values(),
    };
  }
  protected invalidate(): void {
    this.cancelChange();
    this.dirty = false;
  }
  protected handleFrame(frame: ServerFrame): void {
    switch (frame.frame) {
      case "meta_snapshot":
        this.runtimes.replace(frame.runtimes);
        this.projects.replace(frame.projects);
        this.sessions.replace(frame.sessions);
        this.dirty = true;
        this.live();
        this.changed(true);
        break;
      case "mutation": {
        const mutation = frame.mutation;
        if (mutation.type === "runtime") {
          const { type: _tag, ...row } = mutation;
          this.runtimes.set(row);
        } else if (mutation.type === "project") {
          const { type: _tag, ...row } = mutation;
          this.projects.set(row);
        } else if (mutation.type === "session") {
          const { type: _tag, ...row } = mutation;
          this.sessions.set(row);
        } else return;
        this.dirty = true;
        this.changed();
        break;
      }
      case "resync":
        this.invalidate();
        this.setStatus("resyncing");
        break;
      default:
        break;
    }
  }
  protected flushChanges(): void {
    if (this.dirty) {
      this.dirty = false;
      this.handlers.onChange?.(this.state());
    }
  }
  protected onStatus(status: SubscriptionStatus): void {
    this.handlers.onStatus?.(status);
  }
  protected onError(error: ApiError): void {
    this.handlers.onError?.(error);
  }
}

export interface CollaborationSubscriptionHandlers {
  onChange?: (state: CollaborationTimeline) => void;
  onReset?: () => void;
  onStatus?: (status: SubscriptionStatus) => void;
  onError?: (error: ApiError) => void;
}

export class CollaborationSubscription extends BaseSubscription {
  private readonly handlers: CollaborationSubscriptionHandlers;
  private readonly rows = new OrderedCache<TimelineEntry>((r) => r.timeline_id, timelineOrder);
  private readonly observations = new Map<string, ResponseRuntime>();
  private dirty = false;
  constructor(transport: SubscriptionTransport, handlers: CollaborationSubscriptionHandlers) {
    super(transport);
    this.handlers = handlers;
    this.start();
  }
  private observe(observation: ResponseRuntime): void {
    this.observations.set(
      JSON.stringify([observation.response_id, observation.model, observation.effort]),
      observation,
    );
  }
  protected invalidate(): void {
    this.cancelChange();
    this.rows.replace([]);
    this.observations.clear();
    this.dirty = false;
    this.handlers.onReset?.();
  }
  protected handleFrame(frame: ServerFrame): void {
    switch (frame.frame) {
      case "collaboration_snapshot":
        this.invalidate();
        this.rows.replace(frame.entries);
        for (const observation of frame.response_runtimes) this.observe(observation);
        this.dirty = true;
        this.live();
        this.changed(true);
        break;
      case "response_runtime":
        this.observe(frame.observation);
        this.dirty = true;
        this.changed();
        break;
      case "mutation":
        if (frame.mutation.type === "message" && frame.mutation.body.type === "collaboration") {
          const { type: _tag, ...row } = frame.mutation;
          this.rows.set(row);
          this.dirty = true;
          this.changed();
        }
        break;
      case "resync":
        this.invalidate();
        this.setStatus("resyncing");
        break;
      default:
        break;
    }
  }
  protected flushChanges(): void {
    if (this.dirty) {
      this.dirty = false;
      this.handlers.onChange?.({
        entries: this.rows.values(),
        response_runtimes: [...this.observations.values()],
      });
    }
  }
  protected onStatus(status: SubscriptionStatus): void {
    this.handlers.onStatus?.(status);
  }
  protected onError(error: ApiError): void {
    this.handlers.onError?.(error);
  }
}
