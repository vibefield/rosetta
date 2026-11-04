// The room's data spine: one meta subscription for who/what exists, REST for
// figures, and one session subscription for the transcript being read. Every
// hook reads through the port it is handed; none of them dials anything.
import { startTransition, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  ApiError,
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
} from "@vibefield/rosetta-protocol";
import { CursorExpiredError } from "@vibefield/rosetta-protocol";
import type { ProjectStats } from "./model";
import { getActivityGridStartTimestamp, getViewerUtcOffsetMinutes } from "./model";
import { figureRetryDelay } from "./figure-order";
import type { CatalogPort, CollaborationFeed, SessionClient, SessionFeed } from "../ports";
import type { SessionLeases } from "./session-leases";
import { conversationStart, readConversationPage } from "./transcript/history-window";
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
  /** Advances even if React batches a replacement and its first live mutation. */
  snapshotVersion: number;
  status: SubscriptionStatus;
  error: ApiError | null;
}

export interface MetaFeedOptions {
  /** Land each metadata change as a React transition: the renders it causes
   * yield to input, and a burst coalesces into the newest state. The room
   * asks for it while an import runs; what renders is unchanged, only when. */
  readonly transition?: boolean;
}

export function useMeta(catalog: CatalogPort, options: MetaFeedOptions = {}): MetaFeed {
  const [feed, setFeed] = useState<{ meta: MetaState | null; snapshotVersion: number }>({
    meta: null, snapshotVersion: 0,
  });
  const [status, setStatus] = useState<SubscriptionStatus>("connecting");
  const [error, setError] = useState<ApiError | null>(null);
  // Read at delivery, so turning it on or off never resubscribes.
  const transition = useRef(options.transition === true);
  transition.current = options.transition === true;
  useEffect(() => {
    // Status and errors travel the same way as the rows, so a "live" never
    // commits ahead of the snapshot it announces.
    const deliver = (apply: () => void) => {
      if (transition.current) startTransition(apply);
      else apply();
    };
    const sub = catalog.subscribeMeta({
      onChange: (meta, source) =>
        deliver(() => {
          setFeed((previous) => ({
            meta,
            snapshotVersion: previous.snapshotVersion + (source === "snapshot" ? 1 : 0),
          }));
          setError(null);
        }),
      onStatus: (next) => deliver(() => setStatus(next)),
      onError: (next) => deliver(() => setError(next)),
    });
    return () => sub.close();
  }, [catalog]);
  return { ...feed, status, error };
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
 * search preparation, then quietly once both are ready. A connection change,
 * or a new `asked` (the host knows the daemon's state moved: it granted
 * consent), drops the wait and the ask in flight and asks again at once.
 * After a new `asked`, an answer still awaiting consent is the grant not
 * yet visible, not the user choosing: the room asks again every 100 ms, not
 * at the consent pause's 2.5 s, until the phase moves on. */
/** How many 100 ms re-asks a status refresh may make while the grant is unshown. */
const BRISK_REASKS = 50;

export function useDaemonStatus(catalog: CatalogPort, asked = 0): DaemonStatusFeed {
  const [connectionRevision, setConnectionRevision] = useState(0);
  // The last `asked` whose move the daemon has shown.
  const shown = useRef(0);
  useEffect(
    () => catalog.onConnectionChange?.(() => setConnectionRevision((value) => value + 1)),
    [catalog],
  );
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
    // Brisk re-asks after a refresh are bounded: 50 (about 5 s), then the
    // ordinary consent pause, so a grant that never moves the phase costs
    // nothing more than a cadence.
    let briskLeft = BRISK_REASKS;
    const poll = async () => {
      const request = new AbortController();
      controller = request;
      let delay = 1_000;
      deadline = window.setTimeout(() => request.abort(), 5_000);
      try {
        const status = await catalog.status(request.signal);
        if (cancelled) return;
        setFeed({
          connectionError: null,
          canonicalReady: status.canonicalReady,
          searchReadiness: status.searchReadiness,
          ingest: status.ingest,
          searchProgress: status.searchProgress,
          startedAt: status.startedAt,
          uptimeMs: status.uptimeMs,
          receivedAt: Date.now(),
        });
        const phase = status.ingest?.phase;
        if (phase !== "awaiting_consent") shown.current = asked;
        // rosettad answers a grant a moment before its phase leaves
        // awaiting_consent: re-ask briskly until it does.
        const unshown = phase === "awaiting_consent" && shown.current !== asked && briskLeft > 0;
        if (unshown) briskLeft -= 1;
        // Keep search visible after canonical publication. Audit/consent are
        // intentional pauses; an unanswered first request retries promptly.
        const paused = status.ingest?.run?.state === "paused" || status.ingest?.run?.state === "failed" ||
          phase === "metadata_ready" || phase === "awaiting_consent";
        const complete = status.canonicalReady && status.searchReadiness === "ready";
        // An unshown grant comes first: a daemon awaiting consent may still
        // report a complete archive from before, and must not wait 30 s.
        delay = unshown ? 100 : complete ? 30_000 : paused ? 2_500 : 500;
      } catch (error: unknown) {
        if (!cancelled) setFeed((previous) => ({
          ...previous,
          connectionError: catalog.unavailable?.() ??
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
  }, [catalog, connectionRevision, asked]);
  return feed;
}

/** Project figures as they land, and the projects whose latest ask failed. */
export interface ProjectFigures {
  readonly stats: Map<string, ProjectStats>;
  /** Each earns a retry (`figureRetryDelay`); empty when none failed. */
  readonly failed: ReadonlySet<string>;
}

/** Which project to ask next: `first` (the selected project) is read at
 * every ask, so a click mid-hydration jumps the queue; `order` (the list as
 * drawn, `figureOrder`) is read once per round. */
export interface FigurePriority {
  readonly first: string | null;
  readonly order: () => readonly string[];
}

const NO_FAILURES: ReadonlySet<string> = new Set();

/** Per-project heatmaps and contributions. These are global derived reads,
 * so they begin only after the daemon's canonical publication gate. Four
 * workers hydrate the list progressively — the selected project first, then
 * the cards on screen, then the rest (`priority`) — and unbounded
 * Promise.all here once launched two full-corpus queries per project and
 * starved cold ingestion. A project that fails is asked again on its own,
 * with backoff; the others wait for the quiet periodic refresh. */
export function useProjectStats(
  catalog: CatalogPort,
  projectIds: string[],
  ready = false,
  priority?: FigurePriority,
): ProjectFigures {
  const [stats, setStats] = useState<Map<string, ProjectStats>>(new Map());
  const [failed, setFailed] = useState<ReadonlySet<string>>(NO_FAILURES);
  const priorityRef = useRef(priority);
  priorityRef.current = priority;
  const key = projectIds.join("\n");
  useEffect(() => {
    let cancelled = false;
    const controller = new AbortController();
    let timer: number | undefined;
    const ids = key === "" ? [] : key.split("\n");
    if (!ready || ids.length === 0) {
      setStats(new Map());
      setFailed(NO_FAILURES);
      return () => {
        cancelled = true;
        controller.abort();
      };
    }
    const land = (answered: ReadonlyMap<string, ProjectStats>) => {
      if (cancelled || answered.size === 0) return;
      setStats((current) => {
        const next = new Map(current);
        for (const [id, figures] of answered) next.set(id, figures);
        return next;
      });
    };
    /** One ask of every target; resolves to the ones that failed. */
    async function round(targets: readonly string[]): Promise<Set<string>> {
      const span = { sinceMs: getActivityGridStartTimestamp(Date.now()), offsetMinutes: getViewerUtcOffsetMinutes() };
      const missed = new Set<string>();
      // A host that answers every project at once is asked once.
      if (catalog.projectStatsBatch !== undefined) {
        try {
          const answer = await catalog.projectStatsBatch([...targets], span, controller.signal);
          for (const id of targets) if (!answer.has(id)) missed.add(id);
          land(new Map(targets.flatMap((id) => {
            const figures = answer.get(id);
            return figures === undefined ? [] : [[id, figures] as const];
          })));
        } catch {
          // as below: unfigured projects degrade honestly and earn a retry
          for (const id of targets) missed.add(id);
        }
        return missed;
      }
      const left = new Set(targets);
      const drawn = priorityRef.current?.order() ?? [];
      const next = (): string | undefined => {
        const first = priorityRef.current?.first ?? null;
        if (first !== null && left.delete(first)) return first;
        for (const id of drawn) if (left.delete(id)) return id;
        const rest = left.values().next();
        if (rest.done) return undefined;
        left.delete(rest.value);
        return rest.value;
      };
      async function worker() {
        while (!cancelled) {
          const id = next();
          if (id === undefined) return;
          try {
            land(new Map([[id, await catalog.projectStats(id, span, controller.signal)]]));
          } catch {
            // an unfigured project degrades honestly — and earns a retry
            missed.add(id);
          }
        }
      }
      await Promise.all(Array.from({ length: Math.min(4, targets.length) }, () => worker()));
      return missed;
    }
    async function load(targets: readonly string[], failures: number) {
      const missed = await round(targets);
      if (cancelled) return;
      setFailed(missed.size === 0 ? NO_FAILURES : missed);
      // A failed figure (a daemon mid-restart, a dropped socket) is asked
      // again on its own, in seconds and then less often. Stable canonical
      // figures need only a quiet periodic refresh of every project.
      timer = missed.size > 0
        ? window.setTimeout(() => load([...missed], failures + 1), figureRetryDelay(failures + 1))
        : window.setTimeout(() => load(ids, 0), 5 * 60_000);
    }
    load(ids, 0);
    return () => {
      cancelled = true;
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [catalog, key, ready]);
  return useMemo(() => ({ stats, failed }), [stats, failed]);
}

/** The selected project's usage decomposition — per-model days and the
 * hour-of-day profile, over the same 53-week window the heatmap draws. */
export function useProjectUsage(
  catalog: CatalogPort,
  projectId: string | null,
  ready = false,
): UsageResponse | null {
  const [usage, setUsage] = useState<UsageResponse | null>(null);
  useEffect(() => {
    setUsage(null);
    if (projectId === null || !ready) return;
    let cancelled = false;
    const controller = new AbortController();
    const load = () => {
      catalog
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
  }, [catalog, projectId, ready]);
  return usage;
}

export interface TimelineFeed {
  /** Ordinal-ordered rows: paged history + the live tail, deduplicated. */
  rows: TimelineEntry[];
  /** The subscription has delivered its first authoritative snapshot. A live
   * status can arrive one frame earlier, so consumers must not infer an empty
   * transcript until this is true. */
  snapshotReady: boolean;
  /** Initial read failures stay distinct from an authoritative empty snapshot. */
  error: string | null;
  retry: () => void;
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

// @atlas step live.render
export function useTimeline(sessions: SessionLeases, sessionId: string | null): TimelineFeed {
  const [owner, setOwner] = useState(sessionId);
  const [attempt, setAttempt] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [tail, setTail] = useState<TimelineEntry[]>([]);
  const [windowStart, setWindowStart] = useState<number | null>(null);
  const [anchors, setAnchors] = useState<ReadonlySet<string>>(new Set());
  const subscription = useRef<SessionFeed | null>(null);
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
    if (next.older !== previous.older && next.older.length > 0) {
      const sub = subscription.current;
      const combined = [...new Map([...next.older, ...(sub?.entries() ?? [])]
        .map(row => [row.timeline_id, row])).values()].sort((a, b) => a.ordinal - b.ordinal);
      const start = conversationStart(combined, next.cursor !== null);
      // Live arrivals must never move the start forward and hide a visible run.
      setWindowStart(previous => previous === null ? start : Math.min(previous, start ?? previous));
      sub?.reveal(next.older);
    }
  }, []);

  useEffect(() => {
    setOwner(sessionId);
    setError(null);
    setTail([]);
    setWindowStart(null);
    setAnchors(new Set());
    setTimelineMeta(null);
    setWatermark(null);
    setStatus("connecting");
    setSnapshotReady(false);
    sawSnapshot.current = false;
    // A held page belongs to the session it was fetched for.
    apply({ type: "reset" });
    if (sessionId === null) return;
    let active = true;
    let sub: SessionFeed | null = null;
    const lease = sessions.acquire(sessionId);
    const start = (client: SessionClient) => {
      sub = client.subscribeSession(sessionId, {
        onReset: () => {
          if (!active) return;
          setTail([]);
          setWindowStart(null);
          setAnchors(new Set());
          setTimelineMeta(null);
          setSnapshotReady(false);
          setWatermark(null);
          setError(null);
          sawSnapshot.current = false;
          apply({ type: "reset" });
        },
        onSnapshot: (page) => {
          if (!active) return;
          setWindowStart(conversationStart(page.entries, page.cursor !== null));
          apply({ type: "snapshot", cursor: page.cursor });
        },
        onChange: (entries) => {
          if (!active) return;
          setTail(entries);
          setSnapshotReady(true);
          setError(null);
          if (!sawSnapshot.current) {
            sawSnapshot.current = true;
            // Everything in the snapshot is the past; the line goes after it.
            setWatermark(entries.length ? Math.max(...entries.map((entry) => entry.ordinal)) : null);
          }
        },
        onTimelineMetaChange: (meta) => { if (active) setTimelineMeta(meta); },
        onStatus: (status) => {
          if (!active) return;
          setStatus(status);
          if (status === "closed") setError(previous => previous ?? "The message connection closed.");
        },
        onError: (error) => {
          if (!active) return;
          if (error.code === "cursor_expired")
            apply({ type: "expired", seq: modelRef.current.fetchSeq });
          else if (error.code !== "not_ready") setError(error.message);
        },
      });
      subscription.current = sub;
    };
    // A port that answers at once subscribes in this effect, exactly as a
    // bare client did; one that must ask first subscribes when it answers.
    if (lease.client !== null) start(lease.client);
    else {
      lease.ready.then(
        (client) => {
          if (active) start(client);
        },
        (error: unknown) => {
          if (!active) return;
          setStatus("closed");
          setError(error instanceof Error ? error.message : "This session could not be opened.");
        },
      );
    }
    return () => {
      active = false;
      subscription.current = null;
      (sub as SessionFeed | null)?.close();
      lease.release();
    };
  }, [sessions, sessionId, attempt, apply]);

  const retry = useCallback(() => setAttempt(value => value + 1), []);

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
    void readConversationPage(async (cursor, limit) => {
      if (subscription.current !== sub || modelRef.current.fetchSeq !== seq)
        throw new DOMException("The session changed", "AbortError");
      return sub.fetchPage(cursor, limit);
    }, from, EARLIER_PAGE_SIZE).then(
      (page) => {
        if (subscription.current === sub)
          apply({ type: "page", seq, entries: page.entries, cursor: page.cursor });
      },
      (error: unknown) => {
        if (subscription.current !== sub) return;
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
    setAnchors(previous => new Set([...previous, timelineId]));
    try {
      return await sub.loadEntry(timelineId);
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") return false;
      throw error;
    }
  }, []);
  const rows = useMemo(() => windowStart === null ? tail : tail.filter(row =>
    row.ordinal >= windowStart || anchors.has(row.timeline_id)), [tail, windowStart, anchors]);

  const earlier = useMemo(() => earlierState(model, ctx), [model, ctx]);

  // Nothing in the protocol carries a per-session entry total — SessionRow has
  // no count and TimelinePage returns only entries and a cursor — so the head
  // line says the word without the number (§08: "when it is not, the word
  // stands alone"). The arithmetic waits here for the day a total lands.
  const earlierAbove = earlierAboveOf(null, rows.length);

  // Effects replace the subscription after commit. Never render the old
  // session's rows, metadata or readiness under the newly selected title.
  const current = owner === sessionId;

  return {
    rows: current ? rows : [],
    snapshotReady: current && snapshotReady,
    error: current ? error : null,
    retry,
    timelineMeta: current ? timelineMeta : null,
    status: current ? status : "connecting",
    watermark: current ? watermark : null,
    earlier: current ? earlier : { kind: "none" },
    earlierNotice: current ? model.notice : null,
    earlierAbove: current ? earlierAbove : null,
    landed: current ? model.landing : null,
    holdEarlier: startFetch,
    revealEarlier,
    retryEarlier: startFetch,
    dismissEarlierNotice,
    canLoadEarlier: current && earlier.kind !== "none",
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
  readonly historyError: string | null;
  readonly retry: () => void;
}

/** Team conversation is distributed across member transcripts, so one parent
 * document owns a small subscription set. Unlike ordinary Agent subthreads it
 * is eager: delivered boundary messages belong in shared chronology even when
 * no disclosure is open. */
export function useTeamTimelines(
  sessions: SessionLeases,
  sessionIds: readonly string[],
): TeamTimelineFeed {
  const key = [...new Set(sessionIds)].sort().join("\n");
  const ids = useMemo(() => (key === "" ? [] : key.split("\n")), [key]);
  const records = useRef(
    new Map<
      string,
      {
        sub: CollaborationFeed | null;
        history: CollaborationTimeline | null;
        error: string | null;
        release: () => void;
      }
    >(),
  );
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    const wanted = new Set(ids);
    for (const [id, record] of records.current) {
      if (!wanted.has(id)) {
        records.current.delete(id);
        record.sub?.close();
        record.release();
      }
    }
    for (const id of ids) {
      if (records.current.has(id)) continue;
      const lease = sessions.acquire(id);
      const record = {
        sub: null as CollaborationFeed | null,
        history: null as CollaborationTimeline | null,
        error: null as string | null,
        release: () => lease.release(),
      };
      records.current.set(id, record);
      const start = (client: SessionClient) => {
        if (records.current.get(id) !== record) return;
        record.sub = client.subscribeCollaboration(id, {
          onReset: () => {
            if (records.current.get(id) !== record) return;
            record.history = null;
            setRevision((value) => value + 1);
          },
          onChange: (history) => {
            if (records.current.get(id) !== record) return;
            record.history = history;
            record.error = null;
            setRevision((value) => value + 1);
          },
          onError: error => {
            if (records.current.get(id) !== record) return;
            record.error = error.message;
            setRevision(value => value + 1);
          },
          onStatus: status => {
            if (records.current.get(id) !== record || record.history !== null) return;
            if (status === "closed") record.error = "Could not load team messages";
            setRevision(value => value + 1);
          },
        });
      };
      if (lease.client !== null) start(lease.client);
      else {
        lease.ready.then(start, (error: unknown) => {
          if (records.current.get(id) !== record) return;
          record.error = error instanceof Error ? error.message : "Could not load team messages";
          setRevision(value => value + 1);
        });
      }
    }
    setRevision((value) => value + 1);
  }, [sessions, ids]);
  useEffect(
    () => () => {
      const previous = [...records.current.values()];
      records.current.clear();
      previous.forEach((record) => {
        record.sub?.close();
        record.release();
      });
    },
    [],
  );

  const retry = useCallback(() => {
    for (const record of records.current.values()) {
      if (record.history !== null && record.error === null) continue;
      record.error = null;
      record.sub?.restart();
    }
    setRevision(value => value + 1);
  }, []);
  return useMemo(() => {
    const rowsBySession = new Map<string, readonly TimelineEntry[]>();
    const responseRuntimesBySession = new Map<string, readonly ResponseRuntime[]>();
    let historyReady = true;
    let historyError: string | null = null;
    for (const id of ids) {
      const record = records.current.get(id);
      const history = record?.history;
      historyError ??= record?.error ?? null;
      if (!history) {
        historyReady = false;
        continue;
      }
      rowsBySession.set(id, history.entries);
      responseRuntimesBySession.set(id, history.response_runtimes);
    }
    return { rowsBySession, responseRuntimesBySession, historyReady, historyError, retry };
  }, [ids, revision, retry]);
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

// @atlas step search-query.feed
/**
 * One ranked search per settled query. The previous response stays on screen
 * while its successor is in flight — a list that blanks on every keystroke
 * reads as a search that keeps failing — and only the newest request may
 * land: a superseded one is aborted and its late result ignored.
 *
 * A change of `askAgain` re-asks an unchanged query. The room passes whether
 * full text can answer, so the empty answer an unpublished index gave is
 * replaced as soon as the index can answer.
 */
export function useSearch(
  catalog: CatalogPort,
  query: string,
  facets: SearchFacets = {},
  askAgain: string | number | boolean = 0,
): SearchFeed {
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
      catalog
        .search({ q, limit: SEARCH_LIMIT, project, kind, agent }, controller.signal)
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
  }, [catalog, query, project, kind, agent, askAgain]);
  return { query: query.trim(), response, error, searching };
}
