// The room's data spine: one meta subscription for who/what exists, REST for
// figures, and one session subscription for the transcript being read.
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ApiError,
  MessageRow,
  MetaState,
  SearchResponse,
  SubscriptionStatus,
} from "@rosetta/protocol";
import { CursorExpiredError } from "@rosetta/protocol";
import type { ProjectStats } from "./model";
import { viewerOffsetMinutes } from "./model";
import { client } from "./rosetta";

const DAY_MS = 86_400_000;

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

/** Per-project 30-day heatmaps and contributions, refreshed when the project
 * set changes and on a quiet interval afterwards. */
export function useProjectStats(projectIds: string[]): Map<string, ProjectStats> {
  const [stats, setStats] = useState<Map<string, ProjectStats>>(new Map());
  const key = projectIds.join("\n");
  useEffect(() => {
    let cancelled = false;
    const ids = key === "" ? [] : key.split("\n");
    async function loadAll() {
      const sinceMs = Date.now() - 30 * DAY_MS;
      const offsetMinutes = viewerOffsetMinutes();
      const next = new Map<string, ProjectStats>();
      await Promise.all(
        ids.map(async (id) => {
          try {
            const [heat, contribution] = await Promise.all([
              client.heatmap(id, { sinceMs, offsetMinutes }),
              client.contribution(id),
            ]);
            next.set(id, { heat: heat.days, contribution: contribution.rows });
          } catch {
            // a missing project stays unfigured; the row degrades honestly
          }
        }),
      );
      if (!cancelled) setStats(next);
    }
    loadAll();
    const timer = setInterval(loadAll, 60_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [key]);
  return stats;
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
