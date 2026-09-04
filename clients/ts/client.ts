// Hand-written client over the generated protocol types. This is the ONE
// place the subscribe state machine lives outside Rust: snapshot at a
// stamp, apply mutations keyed by timeline_id (including patches into
// earlier rows), obey Resync barriers, and reconnect with a resume stamp.
// Consumers render; they never re-implement protocol semantics.

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
  /** The full ordered timeline after each change. */
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
  return before.type === "workflow"
    && after.type === "workflow"
    && before.run !== null
    && after.run === null
    && before.run_id !== null
    && after.run_id === before.run_id
      ? { ...incoming, action: { ...after, run: before.run } }
      : incoming;
}

export class RosettaClient {
  readonly baseUrl: string;
  private readonly fetchFn: typeof fetch;
  private readonly WebSocketImpl: typeof WebSocket;

  constructor(options: RosettaClientOptions = {}) {
    this.baseUrl = (options.baseUrl ?? DEFAULT_BASE_URL).replace(/\/$/, "");
    this.fetchFn = options.fetchFn ?? fetch.bind(globalThis);
    this.WebSocketImpl = options.WebSocketImpl ?? WebSocket;
  }

  private async response(path: string, init?: RequestInit): Promise<Response> {
    const response = await this.fetchFn(`${this.baseUrl}${path}`, init);
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

  status(): Promise<StatusResponse> {
    return this.request("/api/status");
  }

  /** The roll call's census — what discovery found, content-free. */
  discoveries(): Promise<DiscoveriesResponse> {
    return this.request("/api/discoveries");
  }

  /** Consent: enroll the chosen agent kinds. Grantable exactly once, while
   * the daemon is awaiting consent. */
  enableRuntimes(kinds: string[]): Promise<EnableResponse> {
    return this.request("/api/runtimes/enable", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ kinds }),
    });
  }

  /** Attention-directed ingestion: the reader opened a still-queued
   * session — ask the daemon to read it next. Advisory; safe to fire on
   * every selection of a pending session. */
  boost(sessionId: string): Promise<BoostResponse> {
    return this.request("/api/ingest/boost", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ session_id: sessionId }),
    });
  }

  runtimes(): Promise<RuntimesResponse> {
    return this.request("/api/runtimes");
  }

  projects(): Promise<ProjectsResponse> {
    return this.request("/api/projects");
  }

  sessions(options: { project?: string; limit?: number } = {}): Promise<SessionsResponse> {
    return this.request(`/api/sessions${queryString(options)}`);
  }

  session(id: string): Promise<SessionResponse> {
    return this.request(`/api/sessions/${encodeURIComponent(id)}`);
  }

  /**
   * One backward page. Pass the previous page's `cursor` to continue; a
   * rewrite underneath throws {@link CursorExpiredError} — refetch from the
   * head (no cursor).
   */
  timeline(
    sessionId: string,
    options: { limit?: number; cursor?: string } = {},
  ): Promise<TimelinePage> {
    return this.request(
      `/api/sessions/${encodeURIComponent(sessionId)}/timeline${queryString(options)}`,
    );
  }

  /** Complete delivered collaboration history for one team member. The
   * response deliberately excludes private prose and tool rows. */
  collaborationTimeline(sessionId: string): Promise<CollaborationTimeline> {
    return this.request(
      `/api/sessions/${encodeURIComponent(sessionId)}/collaboration`,
    );
  }

  /** Fetch one exact canonical row for a lazy transcript landing. */
  timelineEntry(sessionId: string, timelineId: string): Promise<TimelineEntry> {
    return this.request(
      `/api/sessions/${encodeURIComponent(sessionId)}/timeline/${encodeURIComponent(timelineId)}`,
    );
  }

  /** Recover one deliberately omitted command body from its raw-ledger
   * record. Nothing is fetched until the reader opens the disclosure. */
  command(sessionId: string, timelineId: string): Promise<CommandResponse> {
    return this.request(
      `/api/sessions/${encodeURIComponent(sessionId)}/timeline/${encodeURIComponent(timelineId)}/command`,
    );
  }

  /** Read the live file named by one canonical file-bearing target. The caller
   * supplies identities and an index, never a path; the daemon resolves the
   * exact path already stored on that timeline row. */
  async readFileTarget(
    sessionId: string,
    timelineId: string,
    targetIndex: number,
  ): Promise<Uint8Array> {
    const response = await this.response(
      `/api/sessions/${encodeURIComponent(sessionId)}/timeline/${encodeURIComponent(timelineId)}/read/${targetIndex}`,
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
    const response = await this.fetchFn(url, { signal, redirect: "error" });
    if (!response.ok) throw new Error(`stored text read failed · HTTP ${response.status}`);
    return response.text();
  }

  /** Read one exact plan document selected from timeline_meta. */
  async readPlan(
    sessionId: string,
    source: TimelineMetaPlanSource,
  ): Promise<Uint8Array> {
    const path =
      source.type === "revision"
        ? `/api/sessions/${encodeURIComponent(sessionId)}/plans/revisions/${source.revision}`
        : `/api/sessions/${encodeURIComponent(sessionId)}/timeline/${encodeURIComponent(source.timeline_id)}/plan`;
    const response = await this.response(path);
    return new Uint8Array(await response.arrayBuffer());
  }

  search(request: SearchRequest): Promise<SearchResponse> {
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
    );
  }

  heatmap(
    projectId: string,
    options: { sinceMs?: number; offsetMinutes?: number } = {},
  ): Promise<HeatmapResponse> {
    return this.request(
      `/api/projects/${encodeURIComponent(projectId)}/heatmap${queryString({
        since_ms: options.sinceMs,
        offset_minutes: options.offsetMinutes,
      })}`,
    );
  }

  contribution(projectId: string): Promise<ContributionResponse> {
    return this.request(`/api/projects/${encodeURIComponent(projectId)}/contribution`);
  }

  /** The heatmap figure's decomposition over the same window: per-model days
   * (they sum back to the heatmap exactly) plus an hour-of-day profile. */
  usage(
    projectId: string,
    options: { sinceMs?: number; offsetMinutes?: number } = {},
  ): Promise<UsageResponse> {
    return this.request(
      `/api/projects/${encodeURIComponent(projectId)}/usage${queryString({
        since_ms: options.sinceMs,
        offset_minutes: options.offsetMinutes,
      })}`,
    );
  }

  subscribeSession(
    sessionId: string,
    handlers: SessionSubscriptionHandlers,
  ): SessionSubscription {
    return new SessionSubscription(
      this.wsUrl(`/ws/sessions/${encodeURIComponent(sessionId)}`),
      this.WebSocketImpl,
      handlers,
    );
  }

  subscribeMeta(handlers: MetaSubscriptionHandlers): MetaSubscription {
    return new MetaSubscription(this.wsUrl("/ws/meta"), this.WebSocketImpl, handlers);
  }

  private wsUrl(path: string): string {
    return `${this.baseUrl.replace(/^http/, "ws")}${path}`;
  }
}

/** Shared reconnecting-socket scaffolding for both topics. */
abstract class BaseSubscription {
  protected stamp: Stamp | null = null;
  private readonly url: string;
  private readonly WebSocketImpl: typeof WebSocket;
  private socket: WebSocket | null = null;
  private attempts = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private closedByUser = false;

  constructor(url: string, WebSocketImpl: typeof WebSocket) {
    this.url = url;
    this.WebSocketImpl = WebSocketImpl;
  }

  /** Called by the subclass constructor AFTER its handlers are assigned —
   * `super()` runs before subclass field initialization, so connecting from
   * the base constructor would emit into undefined handlers. */
  protected start(): void {
    this.connect();
  }

  /** Resume with the last stamp: the server replays the ring gap, or falls
   * back to a fresh snapshot when the position is unreachable. */
  private connect(): void {
    const resume =
      this.stamp === null ? "" : `?epoch=${this.stamp.epoch}&seq=${this.stamp.seq}`;
    this.emitStatus("connecting");
    const socket = new this.WebSocketImpl(`${this.url}${resume}`);
    this.socket = socket;
    socket.onopen = () => {
      this.attempts = 0;
      this.emitStatus("syncing");
    };
    socket.onmessage = (event: MessageEvent) => {
      let frame: ServerFrame;
      try {
        frame = JSON.parse(String(event.data)) as ServerFrame;
      } catch {
        return;
      }
      this.handleFrame(frame);
    };
    socket.onclose = () => {
      if (this.socket !== socket) {
        return;
      }
      this.socket = null;
      if (this.closedByUser) {
        this.emitStatus("closed");
        return;
      }
      const delay = Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** this.attempts);
      this.attempts += 1;
      this.reconnectTimer = setTimeout(() => this.connect(), delay);
    };
    socket.onerror = () => {
      // onclose follows; reconnect is handled there.
    };
  }

  close(): void {
    this.closedByUser = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.socket !== null) {
      this.socket.close();
    } else {
      this.emitStatus("closed");
    }
  }

  protected abstract handleFrame(frame: ServerFrame): void;
  protected abstract emitStatus(status: SubscriptionStatus): void;
}

export class SessionSubscription extends BaseSubscription {
  private readonly handlers: SessionSubscriptionHandlers;
  private readonly rows = new Map<string, TimelineEntry>();
  private timelineMeta: TimelineMeta = { tasks: [], workflows: [], plans: [], artifacts: [] };

  constructor(
    url: string,
    WebSocketImpl: typeof WebSocket,
    handlers: SessionSubscriptionHandlers,
  ) {
    super(url, WebSocketImpl);
    this.handlers = handlers;
    this.start();
  }

  /** The current timeline, ordered by presentation ordinal. */
  entries(): TimelineEntry[] {
    return [...this.rows.values()].sort(
      (a, b) => a.ordinal - b.ordinal || a.timeline_id.localeCompare(b.timeline_id),
    );
  }

  protected handleFrame(frame: ServerFrame): void {
    switch (frame.frame) {
      case "hello":
        this.stamp = frame.stamp;
        break;
      case "timeline_snapshot":
        this.rows.clear();
        for (const row of frame.entries) {
          this.rows.set(row.timeline_id, row);
        }
        this.timelineMeta = normalizeTimelineMeta(frame.timeline_meta);
        this.stamp = frame.stamp;
        this.emitStatus("live");
        this.emitChange();
        this.emitTimelineMetaChange();
        break;
      case "timeline_meta_snapshot":
        this.timelineMeta = normalizeTimelineMeta(frame.timeline_meta);
        if (frame.stamp !== null) {
          this.stamp = frame.stamp;
        }
        this.emitStatus("live");
        this.emitTimelineMetaChange();
        break;
      case "mutation":
        this.apply(frame.mutation);
        if (frame.stamp !== null) {
          this.stamp = frame.stamp;
        }
        // Replay after a resumed reconnect arrives as plain mutations, so
        // the first mutation also means the stream is live again.
        this.emitStatus("live");
        this.emitChange();
        break;
      case "resync":
        // Barrier: a replacement snapshot follows. Keep rendering the stale
        // rows until it lands; the snapshot replaces wholesale.
        this.emitStatus("resyncing");
        break;
      case "meta_snapshot":
        break;
      case "error":
        this.handlers.onError?.(frame.error);
        break;
    }
  }

  /** At-least-once application: every arm is idempotent by timeline_id. */
  private apply(mutation: Mutation): void {
    switch (mutation.type) {
      case "message": {
        const { type: _tag, ...row } = mutation;
        const current = this.rows.get(row.timeline_id);
        const replacement = current?.body.type === "tool_use" && row.body.type === "tool_use"
          ? {
              ...row,
              body: {
                ...row.body,
                tool: retainHydratedWorkflowRun(current.body.tool, row.body.tool),
              },
            }
          : row;
        this.rows.set(row.timeline_id, replacement);
        break;
      }
      case "tool_updated": {
        const row = this.rows.get(mutation.timeline_id);
        if (row?.body.type === "tool_use") {
          this.rows.set(row.timeline_id, {
            ...row,
            body: {
              ...row.body,
              tool: retainHydratedWorkflowRun(row.body.tool, mutation.tool),
            },
          });
        }
        break;
      }
      case "usage": {
        const row = this.rows.get(mutation.timeline_id);
        if (row) {
          this.rows.set(row.timeline_id, {
            ...row,
            usage: mutation.tokens,
            model: mutation.model,
            at: mutation.ts,
          });
        }
        break;
      }
      case "session_swept":
        // Normally surfaced as a Resync barrier instead; defensively treat
        // a direct sweep as "everything you hold is void".
        this.rows.clear();
        break;
      case "timeline_meta_changed":
        // rosettad resolves this internal ring marker into a typed metadata
        // snapshot. Ignore it defensively if an older server forwards it.
        break;
      case "session":
      case "project":
      case "runtime":
        break;
    }
  }

  private emitChange(): void {
    this.handlers.onChange?.(this.entries());
  }

  private emitTimelineMetaChange(): void {
    this.handlers.onTimelineMetaChange?.(this.timelineMeta);
  }

  protected emitStatus(status: SubscriptionStatus): void {
    this.handlers.onStatus?.(status);
  }
}

export class MetaSubscription extends BaseSubscription {
  private readonly handlers: MetaSubscriptionHandlers;
  private readonly runtimes = new Map<string, RuntimeRow>();
  private readonly projects = new Map<string, ProjectRow>();
  private readonly sessions = new Map<string, SessionRow>();

  constructor(url: string, WebSocketImpl: typeof WebSocket, handlers: MetaSubscriptionHandlers) {
    super(url, WebSocketImpl);
    this.handlers = handlers;
    this.start();
  }

  state(): MetaState {
    return {
      runtimes: [...this.runtimes.values()].sort((a, b) => a.id.localeCompare(b.id)),
      projects: [...this.projects.values()].sort((a, b) => a.name.localeCompare(b.name)),
      sessions: [...this.sessions.values()].sort(
        (a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id),
      ),
    };
  }

  protected handleFrame(frame: ServerFrame): void {
    switch (frame.frame) {
      case "hello":
        break;
      case "meta_snapshot":
        this.runtimes.clear();
        this.projects.clear();
        this.sessions.clear();
        for (const runtime of frame.runtimes) {
          this.runtimes.set(runtime.id, runtime);
        }
        for (const project of frame.projects) {
          this.projects.set(project.id, project);
        }
        for (const session of frame.sessions) {
          this.sessions.set(session.id, session);
        }
        this.emitStatus("live");
        this.emitChange();
        break;
      case "mutation": {
        const mutation = frame.mutation;
        if (mutation.type === "runtime") {
          const { type: _tag, ...row } = mutation;
          this.runtimes.set(row.id, row);
        } else if (mutation.type === "project") {
          const { type: _tag, ...row } = mutation;
          this.projects.set(row.id, row);
        } else if (mutation.type === "session") {
          const { type: _tag, ...row } = mutation;
          this.sessions.set(row.id, row);
        } else {
          break;
        }
        this.emitChange();
        break;
      }
      case "resync":
        this.emitStatus("resyncing");
        break;
      case "timeline_snapshot":
      case "timeline_meta_snapshot":
        break;
      case "error":
        this.handlers.onError?.(frame.error);
        break;
    }
  }

  private emitChange(): void {
    this.handlers.onChange?.(this.state());
  }

  protected emitStatus(status: SubscriptionStatus): void {
    this.handlers.onStatus?.(status);
  }
}
