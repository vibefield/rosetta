// The room's data spine: one meta subscription for who/what exists, REST for
// figures, and one session subscription for the transcript being read.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  ApiError,
  SessionSubscription,
  CollaborationSubscription,
  CollaborationTimeline,
  IngestProgress,
  MetaState,
  SearchReadiness,
  SearchBuildProgress,
  SearchResponse,
  ResponseRuntime,
  SubscriptionStatus,
  TimelineEntry,
  TimelineMeta,
  UsageResponse,
} from "@rosetta/protocol";
import { CursorExpiredError } from "@rosetta/protocol";
import type { ProjectStats } from "./model";
import { getActivityGridStartTimestamp, getViewerUtcOffsetMinutes } from "./model";
import { client, desktopDaemonState, onDesktopDaemonState } from "./rosetta";
import type {
  EarlierAction,
  EarlierContext,
  EarlierLanding,
  EarlierModel,
  EarlierNotice,
  EarlierState,
} from "./transcript/head-state";
import {
  EARLIER_FAILED_MESSAGE,
  EARLIER_PAGE_SIZE,
  earlierAboveOf,
  earlierState,
  initialEarlierModel,
  reduceEarlier,
} from "./transcript/head-state";

export interface MetaFeed {
  meta: MetaState | null;
  status: SubscriptionStatus;
  error: ApiError | null;
}

export function useMeta(): MetaFeed {
  const [meta, setMeta] = useState<MetaState | null>(null);
  const [status, setStatus] = useState<SubscriptionStatus>("connecting");
  const [error, setError] = useState<ApiError | null>(null);
  useEffect(() => {
    const sub = client.subscribeMeta({
      onChange: (state) => {
        setMeta(state);
        setError(null);
      },
      onStatus: setStatus,
      onError: setError,
    });
    return () => sub.close();
  }, []);
  return { meta, status, error };
}

export interface DaemonStatusFeed {
  /** A connection or startup failure suitable for the disconnected state. */
  readonly connectionError: string | null;
  /** The exact durable-publication gate for global derived reads. */
  readonly canonicalReady: boolean | null;
  /** Last observed readiness; null until the first successful answer. */
  readonly searchReadiness: SearchReadiness | null;
  /** The cold-start block; null on a daemon too old to carry it. */
  readonly ingest: IngestProgress | null;
  readonly searchProgress: SearchBuildProgress | null;
  readonly startedAt: number | null;
  readonly uptimeMs: number;
  readonly receivedAt: number | null;
}

/** The daemon's status surface. It answers from the daemon's first breath —
 * before the projection is even readable — so it is the room's connectivity
 * truth during a cold start. Polls fast through canonical publication and
 * search preparation, then quietly once both are ready. */
export function useDaemonStatus(): DaemonStatusFeed {
  const [connectionRevision, setConnectionRevision] = useState(0);
  useEffect(() => {
    const changed = () => setConnectionRevision((value) => value + 1);
    const removeConnection = client.onConnectionChange(changed);
    const removeDesktop = onDesktopDaemonState(changed);
    return () => {
      removeConnection();
      removeDesktop();
    };
  }, []);
  const [feed, setFeed] = useState<DaemonStatusFeed>({
    connectionError: null,
    canonicalReady: null,
    searchReadiness: null,
    ingest: null,
    searchProgress: null,
    startedAt: null,
    uptimeMs: 0,
    receivedAt: null,
  });
  useEffect(() => {
    let cancelled = false;
    let controller: AbortController | undefined;
    let timer: number | undefined;
    let deadline: number | undefined;
    const poll = async () => {
      const request = new AbortController();
      controller = request;
      let delay = 1_000;
      deadline = window.setTimeout(() => request.abort(), 5_000);
      try {
        const status = await client.status(request.signal);
        if (cancelled) return;
        setFeed({
          connectionError: null,
          canonicalReady: status.canonical_ready,
          searchReadiness: status.search.readiness,
          ingest: status.daemon?.ingest ?? null,
          searchProgress: status.search.progress ?? null,
          startedAt: status.daemon?.started_at ?? null,
          uptimeMs: status.daemon?.uptime_ms ?? 0,
          receivedAt: Date.now(),
        });
        const phase = status.daemon?.ingest?.phase;
        // Keep search visible after canonical publication. Audit/consent are
        // intentional pauses; an unanswered first request retries promptly.
        const paused = phase === "metadata_ready" || phase === "awaiting_consent";
        const complete = status.canonical_ready && status.search.readiness === "ready";
        delay = complete ? 30_000 : paused ? 2_500 : 500;
      } catch (error: unknown) {
        if (!cancelled) setFeed((previous) => ({
          ...previous,
          connectionError: desktopDaemonState()?.error ??
            (request.signal.aborted ? "Progress updates timed out. Reconnecting…" :
              error instanceof Error ? error.message : "Unable to reach the background service."),
        }));
      } finally {
        window.clearTimeout(deadline);
        if (!cancelled) timer = window.setTimeout(poll, delay);
      }
    };
    void poll();
    return () => {
      cancelled = true;
      controller?.abort();
      window.clearTimeout(deadline);
      window.clearTimeout(timer);
    };
  }, [connectionRevision]);
  return feed;
}

/** Per-project heatmaps and contributions. These are global derived reads,
 * so they begin only after the daemon's canonical publication gate. Four
 * workers hydrate the list progressively; unbounded Promise.all here once
 * launched two full-corpus queries per project and starved cold ingestion. */
export function useProjectStats(projectIds: string[], ready = false): Map<string, ProjectStats> {
  const [stats, setStats] = useState<Map<string, ProjectStats>>(new Map());
  const key = projectIds.join("\n");
  useEffect(() => {
    let cancelled = false;
    const controller = new AbortController();
    let timer: number | undefined;
    const ids = key === "" ? [] : key.split("\n");
    if (!ready || ids.length === 0) {
      setStats(new Map());
      return () => {
        cancelled = true;
        controller.abort();
      };
    }
    async function loadAll() {
      const sinceMs = getActivityGridStartTimestamp(Date.now());
      const offsetMinutes = getViewerUtcOffsetMinutes();
      const queue = [...ids];
      let failed = false;
      async function worker() {
        while (!cancelled) {
          const id = queue.shift();
          if (id === undefined) return;
          try {
            const [heat, contribution] = await Promise.all([
              client.heatmap(id, { sinceMs, offsetMinutes }, controller.signal),
              client.contribution(id, controller.signal),
            ]);
            if (!cancelled) {
              setStats((current) => {
                const next = new Map(current);
                next.set(id, { heat: heat.days, contribution: contribution.rows });
                return next;
              });
            }
          } catch {
            // an unfigured project degrades honestly — and earns a retry
            failed = true;
          }
        }
      }
      await Promise.all(Array.from({ length: Math.min(4, ids.length) }, () => worker()));
      if (cancelled) return;
      // A failed figure (a daemon mid-restart, a dropped socket) retries in
      // seconds. Stable canonical figures need only a quiet periodic refresh.
      timer = window.setTimeout(loadAll, failed ? 8_000 : 5 * 60_000);
    }
    loadAll();
    return () => {
      cancelled = true;
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [key, ready]);
  return stats;
}

/** The selected project's usage decomposition — per-model days and the
 * hour-of-day profile, over the same 53-week window the heatmap draws. */
export function useProjectUsage(projectId: string | null, ready = false): UsageResponse | null {
  const [usage, setUsage] = useState<UsageResponse | null>(null);
  useEffect(() => {
    setUsage(null);
    if (projectId === null || !ready) return;
    let cancelled = false;
    const controller = new AbortController();
    const load = () => {
      client
        .usage(
          projectId,
          {
            sinceMs: getActivityGridStartTimestamp(Date.now()),
            offsetMinutes: getViewerUtcOffsetMinutes(),
          },
          controller.signal,
        )
        .then((res) => {
          if (!cancelled) setUsage(res);
        })
        .catch(() => {
          // the summary panel degrades to what the heatmap alone can say
        });
    };
    load();
    const timer = setInterval(load, 60_000);
    return () => {
      cancelled = true;
      controller.abort();
      clearInterval(timer);
    };
  }, [projectId, ready]);
  return usage;
}

export interface TimelineFeed {
  /** Ordinal-ordered rows: paged history + the live tail, deduplicated. */
  rows: TimelineEntry[];
  /** The subscription has delivered its first authoritative snapshot. A live
   * status can arrive one frame earlier, so consumers must not infer an empty
   * transcript until this is true. */
  snapshotReady: boolean;
  /** Complete task/workflow/plan/artifact index delivered beside the tail snapshot. */
  timelineMeta: TimelineMeta | null;
  status: SubscriptionStatus;
  /** Rows with ordinal > watermark arrived after you did (D33's line). */
  watermark: number | null;
  /** §08 — what the head of the document offers the reader right now. */
  earlier: EarlierState;
  /** The 410 sentence, surfaced once after the paged history is dropped. */
  earlierNotice: EarlierNotice | null;
  /** Rows still above the loaded document, or null when no total is known. */
  earlierAbove: number | null;
  /** The last landing — the receipt's figure and the scroll fix-up's key. */
  landed: EarlierLanding | null;
  /** Fetch the page above and keep it aside. No-op unless idle or failed. */
  holdEarlier: () => void;
  /** Spend the bar: prepend a held page now, or land the fetch on arrival. */
  revealEarlier: () => void;
  /** Try again after `earlier.kind === "failed"`. */
  retryEarlier: () => void;
  /** Drop the 410 sentence once it has been read. */
  dismissEarlierNotice: () => void;
  /** @deprecated §08 replaced the load-earlier button with the head line.
   *  Read `earlier.kind !== "none"` instead. */
  canLoadEarlier: boolean;
  /** @deprecated §08 fetches on approach and inserts on the pull. This fetches
   *  AND inserts in one act, for the anchor search that still pages blindly.
   *  Use `holdEarlier()` then `revealEarlier()`. */
  loadEarlier: () => void;
  /** Load one exact canonical row without walking intervening history pages. */
  loadAnchor: (timelineId: string) => Promise<boolean>;
}

export function useTimeline(sessionId: string | null): TimelineFeed {
  const [tail, setTail] = useState<TimelineEntry[]>([]);
  const subscription = useRef<SessionSubscription | null>(null);
  const [timelineMeta, setTimelineMeta] = useState<TimelineMeta | null>(null);
  const [status, setStatus] = useState<SubscriptionStatus>("connecting");
  const [snapshotReady, setSnapshotReady] = useState(false);
  const [watermark, setWatermark] = useState<number | null>(null);
  const sawSnapshot = useRef(false);

  // The head of the document is a state machine, not a boolean: it holds a
  // page beside the document until the reader pulls (§08, Q7 B). The ref is
  // the authority so a listener firing between commits reduces against the
  // current model; the state mirrors it for rendering.
  const modelRef = useRef<EarlierModel>(initialEarlierModel());
  const [model, setModel] = useState<EarlierModel>(modelRef.current);

  const ctx = useMemo<EarlierContext>(
    () => ({
      tailIds: new Set(tail.map((row) => row.timeline_id)),
      pageSize: EARLIER_PAGE_SIZE,
    }),
    [tail],
  );
  // A pure mirror, so holdEarlier() called from a scroll frame reduces against
  // the tail the reader can actually see.
  const ctxRef = useRef(ctx);
  ctxRef.current = ctx;

  const apply = useCallback((action: EarlierAction) => {
    const previous = modelRef.current;
    const next = reduceEarlier(previous, action, ctxRef.current);
    if (next === previous) return;
    modelRef.current = next;
    setModel(next);
    // The head owns reveal intent and counts. Canonical values live only in
    // the subscription, which patches held pages and visible history together.
    if (next.older !== previous.older && next.older.length > 0)
      subscription.current?.reveal(next.older);
  }, []);

  useEffect(() => {
    setTail([]);
    setTimelineMeta(null);
    setWatermark(null);
    setStatus("connecting");
    setSnapshotReady(false);
    sawSnapshot.current = false;
    // A held page belongs to the session it was fetched for.
    apply({ type: "reset" });
    if (sessionId === null) return;
    const sub = client.subscribeSession(sessionId, {
      onReset: () => {
        setTail([]);
        setTimelineMeta(null);
        setSnapshotReady(false);
        setWatermark(null);
        sawSnapshot.current = false;
        apply({ type: "reset" });
      },
      onSnapshot: (page) => apply({ type: "snapshot", cursor: page.cursor }),
      onChange: (entries) => {
        setTail(entries);
        setSnapshotReady(true);
        if (!sawSnapshot.current) {
          sawSnapshot.current = true;
          // Everything in the snapshot is the past; the line goes after it.
          setWatermark(entries.length ? Math.max(...entries.map((entry) => entry.ordinal)) : null);
        }
      },
      onTimelineMetaChange: setTimelineMeta,
      onStatus: setStatus,
      onError: (error) => {
        if (error.code === "cursor_expired")
          apply({ type: "expired", seq: modelRef.current.fetchSeq });
      },
    });
    subscription.current = sub;
    return () => {
      subscription.current = null;
      sub.close();
    };
  }, [sessionId, apply]);

  /** One fetch of the page above, held rather than inserted. */
  const startFetch = useCallback(() => {
    if (sessionId === null) return;
    const before = modelRef.current.fetchSeq;
    apply({ type: "hold" });
    const started = modelRef.current;
    if (started.phase !== "fetching" || started.fetchSeq === before) return;
    const seq = started.fetchSeq;
    const from = started.cursor;
    const sub = subscription.current;
    if (typeof from !== "string" || sub === null) return;
    void sub.fetchPage(from, EARLIER_PAGE_SIZE).then(
      (page) => apply({ type: "page", seq, entries: page.entries, cursor: page.cursor }),
      (error: unknown) => {
        if (error instanceof CursorExpiredError) apply({ type: "expired", seq });
        else apply({ type: "fail", seq, message: EARLIER_FAILED_MESSAGE });
      },
    );
  }, [sessionId, apply]);

  const revealEarlier = useCallback(() => apply({ type: "reveal" }), [apply]);
  const dismissEarlierNotice = useCallback(() => apply({ type: "clearNotice" }), [apply]);
  const loadEarlier = useCallback(() => {
    startFetch();
    apply({ type: "reveal" });
  }, [startFetch, apply]);
  const loadAnchor = useCallback(async (timelineId: string): Promise<boolean> => {
    const sub = subscription.current;
    if (sub === null) return false;
    try {
      return await sub.loadEntry(timelineId);
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") return false;
      throw error;
    }
  }, []);
  const rows = tail;

  const earlier = useMemo(() => earlierState(model, ctx), [model, ctx]);

  // Nothing in the protocol carries a per-session entry total — SessionRow has
  // no count and TimelinePage returns only entries and a cursor — so the head
  // line says the word without the number (§08: "when it is not, the word
  // stands alone"). The arithmetic waits here for the day a total lands.
  const earlierAbove = earlierAboveOf(null, rows.length);

  return {
    rows,
    snapshotReady,
    timelineMeta,
    status,
    watermark,
    earlier,
    earlierNotice: model.notice,
    earlierAbove,
    landed: model.landing,
    holdEarlier: startFetch,
    revealEarlier,
    retryEarlier: startFetch,
    dismissEarlierNotice,
    canLoadEarlier: earlier.kind !== "none",
    loadEarlier,
    loadAnchor,
  };
}

export interface TeamTimelineFeed {
  /** Complete boundary histories for the known team members. Private work is
   * fetched separately when its report disclosure opens. */
  readonly rowsBySession: ReadonlyMap<string, readonly TimelineEntry[]>;
  /** Content-free response model/effort observations across full history. */
  readonly responseRuntimesBySession: ReadonlyMap<string, readonly ResponseRuntime[]>;
  /** True only after every current member's complete boundary query succeeds. */
  readonly historyReady: boolean;
  /** Confirm that the complete boundary history contains a search landing. */
  readonly loadAnchor: (sessionId: string, timelineId: string) => Promise<boolean>;
}

/** Team conversation is distributed across member transcripts, so one parent
 * document owns a small subscription set. Unlike ordinary Agent subthreads it
 * is eager: delivered boundary messages belong in shared chronology even when
 * no disclosure is open. */
export function useTeamTimelines(sessionIds: readonly string[]): TeamTimelineFeed {
  const key = [...new Set(sessionIds)].sort().join("\n");
  const ids = useMemo(() => (key === "" ? [] : key.split("\n")), [key]);
  const records = useRef(
    new Map<
      string,
      { sub: CollaborationSubscription | null; history: CollaborationTimeline | null }
    >(),
  );
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    const wanted = new Set(ids);
    for (const [id, record] of records.current) {
      if (!wanted.has(id)) {
        records.current.delete(id);
        record.sub?.close();
      }
    }
    for (const id of ids) {
      if (records.current.has(id)) continue;
      const record = {
        sub: null as CollaborationSubscription | null,
        history: null as CollaborationTimeline | null,
      };
      records.current.set(id, record);
      record.sub = client.subscribeCollaboration(id, {
        onReset: () => {
          if (records.current.get(id) !== record) return;
          record.history = null;
          setRevision((value) => value + 1);
        },
        onChange: (history) => {
          if (records.current.get(id) !== record) return;
          record.history = history;
          setRevision((value) => value + 1);
        },
      });
    }
    setRevision((value) => value + 1);
  }, [ids]);
  useEffect(
    () => () => {
      const previous = [...records.current.values()];
      records.current.clear();
      previous.forEach((record) => record.sub?.close());
    },
    [],
  );

  const loadAnchor = useCallback(
    async (sessionId: string, timelineId: string) =>
      records.current
        .get(sessionId)
        ?.history?.entries.some((row) => row.timeline_id === timelineId) ?? false,
    [],
  );
  return useMemo(() => {
    const rowsBySession = new Map<string, readonly TimelineEntry[]>();
    const responseRuntimesBySession = new Map<string, readonly ResponseRuntime[]>();
    let historyReady = true;
    for (const id of ids) {
      const history = records.current.get(id)?.history;
      if (!history) {
        historyReady = false;
        continue;
      }
      rowsBySession.set(id, history.entries);
      responseRuntimesBySession.set(id, history.response_runtimes);
    }
    return { rowsBySession, responseRuntimesBySession, historyReady, loadAnchor };
  }, [ids, revision, loadAnchor]);
}

export interface SearchFeed {
  query: string;
  response: SearchResponse | null;
  error: string | null;
  searching: boolean;
}

/** The structured side of a search: each maps onto one API selector. */
export interface SearchFacets {
  readonly project?: string | null;
  readonly kind?: string | null;
  readonly agent?: string | null;
}

export const SEARCH_LIMIT = 200;

/**
 * One ranked search per settled query. The previous response stays on screen
 * while its successor is in flight — a list that blanks on every keystroke
 * reads as a search that keeps failing — and only the newest request may
 * land: a superseded one is aborted and its late result ignored.
 */
export function useSearch(query: string, facets: SearchFacets = {}): SearchFeed {
  const [response, setResponse] = useState<SearchResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);
  const project = facets.project ?? null;
  const kind = facets.kind ?? null;
  const agent = facets.agent ?? null;
  useEffect(() => {
    const q = query.trim();
    if (q === "") {
      setResponse(null);
      setError(null);
      setSearching(false);
      return;
    }
    const controller = new AbortController();
    let cancelled = false;
    setSearching(true);
    const timer = setTimeout(() => {
      client
        .search(
          {
            q,
            limit: SEARCH_LIMIT,
            session: null,
            runtime: null,
            agent,
            project,
            kind,
            is_error: null,
            after_ts: null,
            before_ts: null,
          },
          controller.signal,
        )
        .then((res) => {
          if (cancelled) return;
          setResponse(res);
          setError(null);
        })
        .catch((err: unknown) => {
          if (cancelled) return;
          setResponse(null);
          setError(err instanceof Error ? err.message : String(err));
        })
        .finally(() => {
          if (!cancelled) setSearching(false);
        });
    }, 180);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      controller.abort();
    };
  }, [query, project, kind, agent]);
  return { query: query.trim(), response, error, searching };
}
