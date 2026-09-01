// The room's data spine: one meta subscription for who/what exists, REST for
// figures, and one session subscription for the transcript being read.
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ApiError,
  IngestProgress,
  MetaState,
  SearchReadiness,
  SearchResponse,
  SubscriptionStatus,
  TimelineEntry,
  UsageResponse,
} from "@rosetta/protocol";
import { CursorExpiredError } from "@rosetta/protocol";
import type { ProjectStats } from "./model";
import { getActivityGridStartTimestamp, getViewerUtcOffsetMinutes } from "./model";
import { client } from "./rosetta";

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
  /** The exact durable-publication gate for global derived reads. */
  readonly canonicalReady: boolean | null;
  /** null until the first answer, and after the daemon stops answering. */
  readonly searchReadiness: SearchReadiness | null;
  /** The cold-start block; null on a daemon too old to carry it. */
  readonly ingest: IngestProgress | null;
}

/** The daemon's status surface. It answers from the daemon's first breath —
 * before the projection is even readable — so it is the room's connectivity
 * truth during a cold start. Polls fast while a backfill runs (the meters
 * are moving), quietly once the phase is live. */
export function useDaemonStatus(): DaemonStatusFeed {
  const [feed, setFeed] = useState<DaemonStatusFeed>({
    canonicalReady: null,
    searchReadiness: null,
    ingest: null,
  });
  const phase = feed.ingest?.phase ?? null;
  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;
    const poll = () => {
      client
        .status()
        .then((status) => {
          if (cancelled) return;
          setFeed({
            canonicalReady: status.canonical_ready,
            searchReadiness: status.search.readiness,
            ingest: status.daemon?.ingest ?? null,
          });
        })
        .catch(() => {
          if (!cancelled)
            setFeed({ canonicalReady: null, searchReadiness: null, ingest: null });
        })
        .finally(() => {
          if (cancelled) return;
          const moving = phase !== null && phase !== "live" && phase !== "metadata_ready";
          timer = window.setTimeout(poll, moving ? 2_500 : 30_000);
        });
    };
    poll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [phase]);
  return feed;
}

/** Per-project heatmaps and contributions. These are global derived reads,
 * so they begin only after the daemon's canonical publication gate. Four
 * workers hydrate the list progressively; unbounded Promise.all here once
 * launched two full-corpus queries per project and starved cold ingestion. */
export function useProjectStats(
  projectIds: string[],
  ready = false,
): Map<string, ProjectStats> {
  const [stats, setStats] = useState<Map<string, ProjectStats>>(new Map());
  const key = projectIds.join("\n");
  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;
    const ids = key === "" ? [] : key.split("\n");
    if (!ready || ids.length === 0) {
      setStats(new Map());
      return () => {
        cancelled = true;
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
              client.heatmap(id, { sinceMs, offsetMinutes }),
              client.contribution(id),
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
      window.clearTimeout(timer);
    };
  }, [key, ready]);
  return stats;
}

/** The selected project's usage decomposition — per-model days and the
 * hour-of-day profile, over the same 53-week window the heatmap draws. */
export function useProjectUsage(
  projectId: string | null,
  ready = false,
): UsageResponse | null {
  const [usage, setUsage] = useState<UsageResponse | null>(null);
  useEffect(() => {
    setUsage(null);
    if (projectId === null || !ready) return;
    let cancelled = false;
    const load = () => {
      client
        .usage(projectId, {
          sinceMs: getActivityGridStartTimestamp(Date.now()),
          offsetMinutes: getViewerUtcOffsetMinutes(),
        })
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
      clearInterval(timer);
    };
  }, [projectId, ready]);
  return usage;
}

export interface TimelineFeed {
  /** Ordinal-ordered rows: paged history + the live tail, deduplicated. */
  rows: TimelineEntry[];
  status: SubscriptionStatus;
  /** Rows with ordinal > watermark arrived after you did (D33's line). */
  watermark: number | null;
  /** Null = no more history; string = next page back; undefined = untried. */
  canLoadEarlier: boolean;
  loadEarlier: () => void;
}

export function useTimeline(sessionId: string | null): TimelineFeed {
  const [tail, setTail] = useState<TimelineEntry[]>([]);
  const [older, setOlder] = useState<TimelineEntry[]>([]);
  const [cursor, setCursor] = useState<string | null | undefined>(undefined);
  const [status, setStatus] = useState<SubscriptionStatus>("connecting");
  const [watermark, setWatermark] = useState<number | null>(null);
  const sawSnapshot = useRef(false);

  useEffect(() => {
    setTail([]);
    setOlder([]);
    setCursor(undefined);
    setWatermark(null);
    setStatus("connecting");
    sawSnapshot.current = false;
    if (sessionId === null) return;
    const sub = client.subscribeSession(sessionId, {
      onChange: (entries) => {
        setTail(entries);
        if (!sawSnapshot.current) {
          sawSnapshot.current = true;
          // Everything in the snapshot is the past; the line goes after it.
          setWatermark(entries.length ? Math.max(...entries.map((entry) => entry.ordinal)) : null);
        }
      },
      onStatus: setStatus,
    });
    return () => sub.close();
  }, [sessionId]);

  const loadEarlier = useMemo(() => {
    return () => {
      if (sessionId === null || cursor === null) return;
      client
        .timeline(sessionId, cursor === undefined ? { limit: 200 } : { limit: 200, cursor })
        .then((page) => {
          setOlder((current) => [...page.entries, ...current]);
          setCursor(page.cursor);
        })
        .catch((error) => {
          if (error instanceof CursorExpiredError) {
            // The projection rewrote underneath: drop paged history and
            // start again from the head rather than silently rebasing.
            setOlder([]);
            setCursor(undefined);
          }
        });
    };
  }, [sessionId, cursor]);

  const rows = useMemo(() => {
    const seen = new Map<string, TimelineEntry>();
    for (const row of older) seen.set(row.timeline_id, row);
    for (const row of tail) seen.set(row.timeline_id, row);
    return [...seen.values()].sort((a, b) => a.ordinal - b.ordinal);
  }, [older, tail]);

  // Untried paging is only on offer when the snapshot may be truncated —
  // a 200-row snapshot that came back short IS the whole history.
  const canLoadEarlier = cursor === undefined ? tail.length >= 200 : cursor !== null;
  return { rows, status, watermark, canLoadEarlier, loadEarlier };
}

export interface SearchFeed {
  query: string;
  response: SearchResponse | null;
  error: string | null;
  searching: boolean;
}

export function useSearch(query: string): SearchFeed {
  const [response, setResponse] = useState<SearchResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);
  useEffect(() => {
    const q = query.trim();
    if (q === "") {
      setResponse(null);
      setError(null);
      setSearching(false);
      return;
    }
    setSearching(true);
    const timer = setTimeout(() => {
      client
        .search({
          q,
          limit: 200,
          session: null,
          runtime: null,
          agent: null,
          project: null,
          kind: null,
          is_error: null,
          after_ts: null,
          before_ts: null,
        })
        .then((res) => {
          setResponse(res);
          setError(null);
        })
        .catch((err: unknown) => {
          setResponse(null);
          setError(err instanceof Error ? err.message : String(err));
        })
        .finally(() => setSearching(false));
    }, 220);
    return () => clearTimeout(timer);
  }, [query]);
  return { query: query.trim(), response, error, searching };
}
