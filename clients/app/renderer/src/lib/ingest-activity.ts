import type { IngestProgress, RuntimeBackfill, SearchBuildProgress, SearchReadiness } from "@rosetta/protocol";

export interface ActivityFeed {
  readonly ingest: IngestProgress | null;
  readonly canonicalReady: boolean | null;
  readonly searchReadiness: SearchReadiness | null;
  readonly searchProgress: SearchBuildProgress | null;
  readonly connectionError: string | null;
  readonly startedAt: number | null;
  readonly uptimeMs: number;
  readonly receivedAt: number | null;
}

export interface ActivityPhase {
  readonly key: string;
  readonly title: string;
  readonly detail: string;
  readonly amount: string;
  readonly step: number;
  readonly current: number;
  readonly total: number;
  readonly unit: "bytes" | "files" | "documents" | "records" | null;
  readonly fraction: number | null;
  readonly active: boolean;
}

const number = (value: number) => value.toLocaleString();
export const fraction = (done: number, total: number) => total > 0
  ? Math.max(0, Math.min(1, done / total)) : null;

export function progressBytes(value: number): string {
  if (value < 1_000) return `${Math.max(0, Math.round(value))} B`;
  if (value < 1_000_000) return `${(value / 1_000).toFixed(0)} KB`;
  if (value < 1_000_000_000) return `${(value / 1_000_000).toFixed(1)} MB`;
  return `${(value / 1_000_000_000).toFixed(2)} GB`;
}

export function progressDuration(ms: number): string {
  const seconds = Math.floor(Math.max(0, ms) / 1_000);
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3_600) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  return `${Math.floor(seconds / 3_600)}h ${Math.floor((seconds % 3_600) / 60)}m`;
}

/** Percentages describe the named step, never an invented overall ETA. */
export function activityPhase(feed: ActivityFeed): ActivityPhase | null {
  const ingest = feed.ingest;
  if (ingest?.phase === "awaiting_consent" || feed.receivedAt === null) return null;
  const runtimes = ingest?.runtimes ?? [];
  const sessions = runtimes.reduce((sum, row) => sum + row.sessions_total, 0);
  const base: ActivityPhase = {
    key: "discovering", title: "Finding your history", step: 1,
    detail: "Checking the agents on this Mac. Your history stays on this device.",
    amount: "Looking for conversations…", current: 0, total: 0,
    unit: null, fraction: null, active: true,
  };
  if (ingest?.phase === "discovering") return base;
  if (ingest?.phase === "restoring") {
    const current = runtimes.filter((row) => row.state === "streaming")
      .reduce((sum, row) => sum + row.records, 0);
    return { ...base, key: "restoring", title: "Restoring saved progress",
      detail: "Recovering saved conversations so your import can continue.",
      amount: `${number(current)} items restored`, current, unit: "records" };
  }
  if (ingest?.phase === "metadata_ready") return {
    ...base, key: "paused", title: "History scan complete", active: false,
    detail: "Transcript import is paused after scanning your history.",
    amount: `${number(sessions)} sessions found`,
  };
  if (ingest?.phase === "finalizing" || ingest?.phase === "publishing") return {
    ...base, key: ingest.phase, step: 3,
    title: ingest.phase === "publishing" ? "Saving your archive" : "Finishing your archive",
    detail: ingest.phase === "publishing"
      ? "Saving your history to this device. Search is next."
      : "Checking conversations and preparing your activity summaries.",
    amount: `${number(sessions)} sessions imported`,
  };
  if (ingest && ingest.phase !== "live") {
    const scanning = runtimes.length === 0 || runtimes.some((row) => row.state === "skimming") ||
      runtimes.every((row) => row.state === "queued");
    if (scanning) {
      const current = runtimes.reduce((sum, row) => sum + row.files_skimmed, 0);
      const total = runtimes.reduce((sum, row) => sum + row.files_total, 0);
      return {
        ...base, key: "metadata", title: "Discovering conversations",
        detail: `${number(sessions)} sessions found. Projects appear as each agent is scanned.`,
        amount: `${number(current)} files checked`, current, total, unit: "files",
        fraction: fraction(current, total),
      };
    }
    if (runtimes.some((row) => row.state === "streaming")) {
      const current = runtimes.reduce((sum, row) => sum + row.bytes_streamed, 0);
      const total = runtimes.some((row) => row.bytes_total === 0 && row.sessions_total > 0) ? 0
        : runtimes.reduce((sum, row) => sum + row.bytes_total, 0);
      return {
        ...base, key: "reading", title: "Reading conversations", step: 2,
        detail: "You can browse conversations as each agent becomes ready.",
        amount: `${progressBytes(current)} processed`, current, total, unit: "bytes",
        fraction: fraction(current, total),
      };
    }
    return {
      ...base, key: "organizing", title: "Organizing conversations", step: 3,
      detail: "Putting messages and tool activity in order before they become available.",
      amount: `${number(sessions)} sessions found`,
    };
  }
  if (feed.searchReadiness === "failed") return {
    ...base, key: "search_failed", title: "Search needs attention", step: 4, active: false,
    detail: "Your conversations are available. Search will retry automatically.",
    amount: "Ready to browse",
  };
  if (feed.canonicalReady && feed.searchReadiness === "ready") return {
    ...base, key: "ready", title: "Your archive is ready", step: 4, active: false,
    detail: "Browse and search all your conversations.",
    amount: `${number(sessions)} sessions imported`, fraction: 1,
  };
  // Routine live repair must not reopen the first-run import panel.
  if (feed.canonicalReady && !feed.searchProgress &&
    (feed.searchReadiness === "catching-up" || feed.searchReadiness === "stale")) return null;
  if (feed.canonicalReady) {
    const progress = feed.searchProgress;
    const indexing = !progress || progress.stage === "indexing";
    return {
      ...base, key: `search_${progress?.stage ?? "waiting"}`, step: 4,
      title: indexing ? "Preparing search" : "Finishing search",
      detail: indexing ? "Your conversations are ready to browse. Search will be available shortly."
        : progress.stage === "publishing" ? "Saving the search index. Your conversations are ready to browse."
        : "Checking the search index. Your conversations are ready to browse.",
      amount: progress ? `${number(progress.documents_done)} of ${number(progress.documents_total)} messages`
        : "Ready to browse",
      current: progress?.documents_done ?? 0, total: progress?.documents_total ?? 0,
      unit: indexing ? "documents" : null,
      fraction: indexing && progress ? fraction(progress.documents_done, progress.documents_total) : null,
    };
  }
  return base;
}

export function runtimeActivity(row: RuntimeBackfill, restoring = false): { label: string; fraction: number | null } {
  if (restoring && row.state === "streaming") return { label: `${number(row.records)} items restored`, fraction: null };
  switch (row.state) {
    case "queued": return { label: row.sessions_total ? "Waiting to read" : "Waiting to scan", fraction: null };
    case "skimming": return { label: `${number(row.sessions_total)} sessions found`, fraction: fraction(row.files_skimmed, row.files_total) };
    case "streaming": return { label: `${progressBytes(row.bytes_streamed)} processed`, fraction: fraction(row.bytes_streamed, row.bytes_total) };
    case "preparing": return { label: "Organizing conversations", fraction: null };
    case "applying": return { label: "Making conversations available", fraction: null };
    case "done": return { label: "Ready to browse", fraction: 1 };
  }
}

export interface ActivitySamples {
  readonly identity: string;
  readonly points: readonly { at: number; value: number }[];
  readonly changedAt: number;
  readonly rate: number | null;
}

/** Eight seconds of actual observations smooth variable batch sizes. A restart,
 * phase transition, retry, or regressing counter starts a new sample window. */
export function sampleActivity(previous: ActivitySamples | null, feed: ActivityFeed, phase: ActivityPhase): ActivitySamples {
  const at = feed.receivedAt ?? 0;
  const identity = `${feed.startedAt}:${phase.key}:${phase.unit}`;
  const last = previous?.points.at(-1);
  if (!previous || previous.identity !== identity || !last || phase.current < last.value || at < last.at) {
    return { identity, points: [{ at, value: phase.current }], changedAt: at, rate: null };
  }
  if (at === last.at || feed.connectionError) return previous;
  const points = [...previous.points.filter((point) => at - point.at <= 8_000), { at, value: phase.current }];
  const first = points[0];
  const changedAt = phase.current !== last.value ? at : previous.changedAt;
  const rate = at - first.at >= 1_000 && at - changedAt < 4_000
    ? (phase.current - first.value) * 1_000 / (at - first.at) : null;
  return { identity, points, changedAt, rate: rate !== null && rate > 0 ? rate : null };
}

export interface IngestActivity {
  readonly phase: ActivityPhase;
  readonly feed: ActivityFeed;
  readonly footer: string;
  readonly elapsed: string;
}
