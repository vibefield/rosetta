// The room's reading of a rosetta status answer. A host that holds the
// daemon's own `StatusResponse` hands it through this; a host with its own
// status doors builds a `CatalogStatus` from them instead. Tolerant: an older
// daemon's missing blocks read as absent, never as a failure.
import type { StatusResponse } from "@vibefield/rosetta-protocol";
import type { CatalogStatus } from "../ports";

export function catalogStatusOf(status: StatusResponse): CatalogStatus {
  return {
    canonicalReady: status.canonical_ready,
    searchReadiness: status.search.readiness,
    searchProgress: status.search.progress ?? null,
    ingest: status.daemon?.ingest ?? null,
    startedAt: status.daemon?.started_at ?? null,
    uptimeMs: status.daemon?.uptime_ms ?? 0,
  };
}
