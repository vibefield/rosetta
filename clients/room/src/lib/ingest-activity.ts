import type { IngestProgress, RuntimeBackfill, SearchBuildProgress, SearchReadiness } from "@vibefield/rosetta-protocol";
import type { ConversationCount, ConversationHorizons, ConversationReadiness } from "./conversation-readiness";

export interface ActivityFeed {
  readonly ingest: IngestProgress | null;
  readonly canonicalReady: boolean | null;
  readonly searchReadiness: SearchReadiness | null;
  readonly searchProgress: SearchBuildProgress | null;
  readonly connectionError: string | null;
  readonly startedAt: number | null;
  readonly uptimeMs: number;
  readonly receivedAt: number | null;
  /** This daemon start's attempt is a resumed build (`resumedRun`).
   * A resumed build's board counts nothing done until it publishes, while the
   * conversations a reader could open before stay open, so its card counts
   * nothing. Absent means a fresh build. */
  readonly resumed?: boolean;
  /** The readable conversations, folded from the pushed catalog in the same
   * render as the rows (`ConversationFold`); absent counts nothing. */
  readonly conversations?: ConversationReadiness | null;
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
  /** Drawn after the title (`phaseHeading`), " · 412 of 1,139 conversations
   * readable"; absent, or null, when there is nothing to count. */
  readonly count?: string | null;
  /** What the card's live region says for this step when the title carries
   * a number, so it announces the step and never each count; absent: the
   * title. */
  readonly spoken?: string;
  /** The recency line under the meter (`horizonLine`); absent: none. */
  readonly horizon?: Horizon | null;
}

export interface Horizon {
  /** "Last 24 hours: 18 of 24 readable", "Last 7 days ready ✓". */
  readonly line: string;
  /** The last horizon reached, the only words its live region announces;
   * null before the first. */
  readonly reached: string | null;
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

const plural = (n: number, noun: string) => `${noun}${n === 1 ? "" : "s"}`;

/** "412 of 1,139 conversations readable". */
export function readableText({ readable, total }: ConversationCount): string {
  return `${number(readable)} of ${number(total)} ${plural(total, "conversation")} readable`;
}

/** The readable conversations the card counts (`data-ready-count`) and their
 * total (`data-ready-total`): root sessions from the pushed catalog, never a
 * metadata-only placeholder. */
export function readyConversations(feed: ActivityFeed): ConversationCount {
  return { readable: feed.conversations?.readable ?? 0, total: feed.conversations?.total ?? 0 };
}

/**
 * The recency line: the last 24 hours until every conversation in it is
 * readable, then the last 7 days. Spans run back from the reader's clock
 * over each root's `updated_at` (its source's freshness, not its activity),
 * so the words are about the conversations themselves. A conversation no
 * import reaches keeps its span open, and the line says so by its count.
 */
export function horizonLine({ day, week }: ConversationHorizons): Horizon | null {
  const dayDone = day.total > 0 && day.readable >= day.total;
  const weekDone = week.total > 0 && week.readable >= week.total;
  const reached = weekDone ? "Last 7 days ready" : dayDone ? "Last 24 hours ready" : null;
  if (day.total > 0 && !dayDone) {
    return { line: `Last 24 hours: ${number(day.readable)} of ${number(day.total)} readable`, reached };
  }
  if (week.total > 0 && !weekDone) {
    return { line: `${dayDone ? "Last 24 hours ready ✓ · " : ""}Last 7 days: ${number(week.readable)} of ${number(week.total)}`, reached };
  }
  return weekDone ? { line: "Last 7 days ready ✓", reached } : null;
}

/** The title as drawn: "Importing your newest conversations first · 412 of
 * 1,139 conversations readable". */
export function phaseHeading(phase: ActivityPhase): string {
  return `${phase.title}${phase.count ?? ""}`;
}

/** The run key while this daemon start's attempt is a resumed build, until
 * the import goes live: a resumed build reads, reconciles and publishes as
 * one, so its card says so. The daemon says so itself: a runtime whose
 * course is `at_publication` (across a launch or after a pause). A daemon
 * that names no course is known resumed once its run is seen restoring; a
 * room that opens after that phase cannot tell, and reads the fresh copy. */
export function resumedRun(previous: string | null, feed: ActivityFeed): string | null {
  const ingest = feed.ingest;
  if (ingest === null || ingest.phase === "live") return null;
  const key = `${feed.startedAt ?? ""}:${ingest.run?.id ?? ""}`;
  const courses = ingest.runtimes.flatMap((row) => (row.admission_course == null ? [] : [row.admission_course]));
  if (courses.length > 0) return courses.includes("at_publication") ? key : null;
  if (ingest.phase === "restoring") return key;
  return previous === key ? key : null;
}

const BORN_DURING_IMPORT = "Conversations you start now appear when the import finishes.";
const ARCHIVE_PAUSE = "Conversations may pause for a few seconds while the archive is saved.";

/** What the card says while the service does not answer: its title, and the
 * line under it (the hold says the same under a host's mark, R10). */
export const PAUSED_UPDATES = {
  title: "Progress updates paused",
  detail: "Waiting for the background service. Reconnecting automatically…",
} as const;

/** Percentages describe the named step, never an invented overall ETA. */
export function activityPhase(feed: ActivityFeed): ActivityPhase | null {
  const ingest = feed.ingest;
  if (feed.receivedAt === null) return null;
  const runtimes = ingest?.runtimes ?? [];
  const sessions = runtimes.reduce((sum, row) => sum + row.sessions_total, 0);
  const base: ActivityPhase = {
    key: "discovering", title: "Finding your history", step: 1,
    detail: "Checking the agents on this Mac. Your history stays on this device.",
    amount: "Looking for conversations…", current: 0, total: 0,
    unit: null, fraction: null, active: true,
  };
  const run = ingest?.run;
  if (run?.state === "paused" || run?.state === "pausing" || run?.state === "failed") {
    const paused = run.state === "paused";
    const failed = run.state === "failed";
    return { ...base, key: run.state, active: !paused && !failed,
      title: paused ? "History import paused" : failed ? "History import needs attention" : "Pausing history import",
      detail: paused ? "Saved progress is retained. Resume when you are ready."
        : failed ? run.error ?? "Import stopped. You can resume after resolving the problem."
        : "Finishing in-flight work and saving progress. Your history stays available.",
      amount: `${number(sessions)} sessions found` };
  }
  if (ingest?.phase === "awaiting_consent") return null;
  if (ingest?.phase === "discovering") return base;
  if (ingest?.phase === "restoring") {
    const current = runtimes.filter((row) => row.state === "streaming")
      .reduce((sum, row) => sum + row.records, 0);
    // The board starts again at zero here, so the copy counts nothing: any
    // conversation a reader could open before stays open.
    return { ...base, key: "restoring", title: "Restoring saved progress",
      detail: "Picking up where the import stopped. Conversations you can open now stay open.",
      amount: `${number(current)} items restored`, current, unit: "records" };
  }
  if (ingest?.phase === "metadata_ready") return {
    ...base, key: "paused", title: "History scan complete", active: false,
    detail: "Transcript import is paused after scanning your history.",
    amount: `${number(sessions)} sessions found`,
  };
  const conversations = feed.conversations ?? null;
  const counted = conversations !== null && conversations.total > 0 ? conversations : null;
  const allReadable = counted !== null && counted.readable >= counted.total;
  const count = counted === null ? null : ` · ${readableText(counted)}`;
  // Canonical readiness comes while the phase still says publishing: the
  // archive is installed by then, so the card moves on to search.
  const published = feed.canonicalReady === true && ingest?.phase === "publishing";
  if (ingest?.phase === "finalizing" && allReadable) return {
    ...base, key: ingest.phase, step: 3,
    title: `All ${number(counted.total)} ${plural(counted.total, "conversation")} readable`,
    spoken: "All conversations readable",
    detail: `Finishing the archive so search and charts can start. ${ARCHIVE_PAUSE}`,
    amount: `${number(sessions)} sessions imported`,
  };
  if (ingest?.phase === "finalizing" || (ingest?.phase === "publishing" && !published)) return {
    ...base, key: ingest.phase, step: 3,
    title: ingest.phase === "publishing" ? "Saving your archive" : "Finishing your archive",
    count: ingest.phase === "finalizing" ? count : null,
    detail: ingest.phase === "publishing"
      ? `Saving your history to this device. ${ARCHIVE_PAUSE}`
      : `Finishing the archive so search and charts can start. ${ARCHIVE_PAUSE}`,
    amount: `${number(sessions)} sessions imported`,
  };
  if (ingest && ingest.phase !== "live" && !published) {
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
    // Recency spans follow a fresh build only: a resumed one opens nothing
    // new before it publishes.
    const horizon = counted === null || feed.resumed ? null : horizonLine(counted.horizons);
    if (runtimes.some((row) => row.state === "streaming")) {
      const current = runtimes.reduce((sum, row) => sum + row.bytes_streamed, 0);
      const total = runtimes.some((row) => row.bytes_total === 0 && row.sessions_total > 0) ? 0
        : runtimes.reduce((sum, row) => sum + row.bytes_total, 0);
      return {
        ...base, key: "reading", step: 2,
        // Newest trees import first and each opens as it closes. A
        // resumed build opens nothing new before it publishes; what a
        // reader could open before stays open, and the count says so.
        title: feed.resumed ? "Finishing the import" : "Importing your newest conversations first",
        count, horizon,
        detail: feed.resumed
          ? "Conversations you can open now stay open. The rest, and any you start now, appear when the import finishes."
          : `Older history fills in while you browse. ${BORN_DURING_IMPORT}`,
        amount: `${progressBytes(current)} processed`, current, total, unit: "bytes",
        fraction: fraction(current, total),
      };
    }
    // Reading is over and the agents are closing. A conversation no source
    // rosters stays Pending past them, so the copy never promises that
    // every one left opens here.
    if (allReadable) return {
      ...base, key: "organizing", step: 3,
      title: `All ${number(counted.total)} ${plural(counted.total, "conversation")} readable`,
      spoken: "All conversations readable",
      detail: "Finishing the archive so search and charts can start.",
      amount: `${number(sessions)} sessions found`,
    };
    return {
      ...base, key: "organizing", step: 3,
      title: feed.resumed ? "Finishing the import" : "Opening conversations as each agent finishes",
      count, horizon,
      detail: feed.resumed
        ? "Conversations you can open now stay open. The rest, and any you start now, appear when the import finishes."
        : `Conversations still importing open as their agent finishes. ${BORN_DURING_IMPORT}`,
      amount: `${number(sessions)} sessions found`,
    };
  }
  if (feed.searchReadiness === "failed") return {
    ...base, key: "search_failed", title: "Search needs attention", step: 4, active: false,
    detail: "Your conversations are available. Search will retry automatically.",
    amount: "Ready to browse",
  };
  // The end, one line: the card then folds away (`useIngestActivity`).
  if (feed.canonicalReady && feed.searchReadiness === "ready") return {
    ...base, key: "ready", title: "Search ready", step: 4, active: false,
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

export function activityIdentity(feed: ActivityFeed, phase: ActivityPhase): string {
  return `${feed.startedAt}:${feed.ingest?.run?.progress_epoch ?? ""}:${phase.key}:${phase.unit}`;
}

/** Eight seconds of actual observations smooth variable batch sizes. A restart,
 * phase transition, retry, or regressing counter starts a new sample window. */
export function sampleActivity(previous: ActivitySamples | null, feed: ActivityFeed, phase: ActivityPhase): ActivitySamples {
  const at = feed.receivedAt ?? 0;
  const identity = activityIdentity(feed, phase);
  const last = previous?.points.at(-1);
  if (!previous || previous.identity !== identity || !last || phase.current < last.value || at < last.at) {
    return { identity, points: [{ at, value: phase.current }], changedAt: at, rate: null };
  }
  if (at === last.at || feed.connectionError) return previous;
  const points = [...previous.points.filter((point) => at - point.at <= 8_000), { at, value: phase.current }];
  const first = points[0]!;
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
  /** The finished card is folding its height to zero, then goes. */
  readonly leaving?: boolean;
}
