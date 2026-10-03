import { useEffect, useState } from "react";
import type { DayUsageResponse } from "@rosetta/protocol";
import { useRoom } from "../room-context";

type Result = { key: string; status: "loading" | "ready" | "error"; data: DayUsageResponse | null };

/** Requests follow selection, not hover. Superseded requests cannot replace
 * the day being read, including during retry or project changes. */
export function useDayUsage(projectId: string, day: string | null, offsetMinutes: number) {
  const key = JSON.stringify([projectId, day, offsetMinutes]);
  const [result, setResult] = useState<Result | null>(null);
  const [revision, setRevision] = useState(0);
  const { catalog } = useRoom();
  useEffect(() => {
    if (!day) return;
    let cancelled = false;
    const controller = new AbortController();
    setResult({ key, status: "loading", data: null });
    catalog.dayUsage(projectId, day, offsetMinutes, controller.signal).then(data => {
      if (!cancelled) setResult({ key, status: data.day === day ? "ready" : "error",
        data: data.day === day ? data : null });
    }).catch(() => {
      if (!cancelled) setResult({ key, status: "error", data: null });
    });
    return () => { cancelled = true; controller.abort(); };
  }, [catalog, projectId, day, offsetMinutes, key, revision]);
  return {
    status: result?.key === key ? result.status : "loading" as const,
    data: result?.key === key ? result.data : null,
    retry: () => setRevision(value => value + 1),
  };
}
