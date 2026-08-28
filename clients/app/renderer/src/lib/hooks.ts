// The room's data spine: one meta subscription for who/what exists, REST for
// figures, and one session subscription for the transcript being read.
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ApiError,
  IngestProgress,
  MessageRow,
  MetaState,
  SearchReadiness,
  SearchResponse,
  SubscriptionStatus,
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
            searchReadiness: status.search.readiness,
            ingest: status.daemon?.ingest ?? null,
          });
        })
        .catch(() => {
          if (!cancelled) setFeed({ searchReadiness: null, ingest: null });
        })
        .finally(() => {
          if (cancelled) return;
          timer = window.setTimeout(poll, phase !== null && phase !== "live" ? 2_500 : 30_000);
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

/** Per-project heatmaps (the calendar heatmap's 53-week window — the ledger's 30-day
 * figures are its tail) and contributions, refreshed when the project set
 * changes and on a quiet interval afterwards. `fast` (a running backfill)
 * tightens the quiet interval so figures visibly grow under the veils. */
export function useProjectStats(
  projectIds: string[],
  fast = false,
): Map<string, ProjectStats> {
  const [stats, setStats] = useState<Map<string, ProjectStats>>(new Map());
  const key = projectIds.join("\n");
  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;
    const ids = key === "" ? [] : key.split("\n");
    async function loadAll() {
      const sinceMs = getActivityGridStartTimestamp(Date.now());
      const offsetMinutes = getViewerUtcOffsetMinutes();
      const next = new Map<string, ProjectStats>();
      let failed = false;
      await Promise.all(
        ids.map(async (id) => {
          try {
            const [heat, contribution] = await Promise.all([
              client.heatmap(id, { sinceMs, offsetMinutes }),
              client.contribution(id),
            ]);
            next.set(id, { heat: heat.days, contribution: contribution.rows });
          } catch {
            // an unfigured project degrades honestly — and earns a retry
            failed = true;
          }
        }),
      );
      if (cancelled) return;
      setStats(next);
      // A failed figure (a daemon mid-restart, a dropped socket) retries in
      // seconds, not at the next quiet refresh a minute out.
      timer = window.setTimeout(loadAll, failed ? 8_000 : fast ? 5_000 : 60_000);
    }
    loadAll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [key, fast]);
  return stats;
}

/** The selected project's usage decomposition — per-model days and the
 * hour-of-day profile, over the same 53-week window the heatmap draws. */
export function useProjectUsage(
  projectId: string | null,
  fast = false,
): UsageResponse | null {
  const [usage, setUsage] = useState<UsageResponse | null>(null);
  useEffect(() => {
    setUsage(null);
    if (projectId === null) return;
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
    const timer = setInterval(load, fast ? 5_000 : 60_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [projectId, fast]);
  return usage;
}

export interface TimelineFeed {
  /** Ordinal-ordered rows: paged history + the live tail, deduplicated. */
  rows: MessageRow[];
  status: SubscriptionStatus;
  /** Rows with ordinal > watermark arrived after you did (D33's line). */
  watermark: number | null;
  /** Null = no more history; string = next page back; undefined = untried. */
  canLoadEarlier: boolean;
  loadEarlier: () => void;
}

export function useTimeline(sessionId: string | null): TimelineFeed {
  const [tail, setTail] = useState<MessageRow[]>([]);
  const [older, setOlder] = useState<MessageRow[]>([]);
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
      onChange: (messages) => {
        setTail(messages);
        if (!sawSnapshot.current) {
          sawSnapshot.current = true;
          // Everything in the snapshot is the past; the line goes after it.
          setWatermark(messages.length ? Math.max(...messages.map((m) => m.ordinal)) : null);
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
          setOlder((current) => [...page.messages, ...current]);
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
    const seen = new Map<string, MessageRow>();
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
