// R11 — the import as the room tells it to a host that draws its sign in its
// own chrome (VibeField: a circle beside its search, with a panel under it).
// Plain data — words, figures and fractions, never a port, a node or a
// callback — read from the same `activityPhase` the card draws, so the sign
// and the card never disagree. A percentage names its step, never an invented
// whole; after the import the sign stays, at rest, and says what is kept.
import type { RuntimeBackfill, RuntimeIngestStatus } from "@vibefield/rosetta-protocol";
import {
  activityPhase, PAUSED_UPDATES, progressBytes, readingSizes, readyConversations, runtimeActivity,
  runtimeName, unsizedText,
} from "./ingest-activity";
import type { ActivityFeed, ActivityPhase, IngestActivity } from "./ingest-activity";

/** `working`: the import moves. `paused`: it waits — a pause, a scan that
 * stopped at its census, or a service that has stopped answering. `attention`:
 * it stopped (`attention: "import"`) or search failed (`"search"`). `done`:
 * "Search ready", while the card would show it. `rest`: up to date. */
export type ImportSignTone = "working" | "paused" | "attention" | "done" | "rest";

export interface ImportSign {
  readonly tone: ImportSignTone;
  /** The step the import is on, 1–4 ("Step 2 of 4"); 4 once it is done. */
  readonly step: 1 | 2 | 3 | 4;
  /** The step's own fraction while it knows one; 1 once done. */
  readonly fraction: number | null;
  /** Over the title: "Importing history · step 2 of 4", "History · up to date". */
  readonly eyebrow: string;
  readonly title: string;
  /** The title's count, "412 of 1,139 conversations readable"; null when none. */
  readonly count: string | null;
  readonly detail: string;
  /** The recency line, "Last 24 hours: 18 of 24 readable"; null when none. */
  readonly horizon: string | null;
  /** What needs attention: the import (it stopped) or search; null otherwise. */
  readonly attention: "import" | "search" | null;
  /** The sign's accessible name, which is also its tooltip. */
  readonly label: string;
  /** The four steps, each with what it opens. */
  readonly steps: readonly ImportSignStep[];
  /** Each agent, in the index's serve order. */
  readonly agents: readonly ImportSignAgent[];
  /** What the index keeps on this machine; null from an index that does not
   * say, or before its first measurement. */
  readonly space: ImportSignSpace | null;
  /** The step's rate ("1.2 MB/s"), else the connection's word. */
  readonly footer: string;
  /** How long the import has run ("3m 12s elapsed"); null at rest. */
  readonly elapsed: string | null;
}

export interface ImportSignStep {
  readonly state: "done" | "current" | "later";
  /** What the step opens: "Projects and sessions", "Open any conversation",
   * "Charts and figures", "Search everything". */
  readonly opens: string;
  /** Its figure: "8,214 of 21,400 files", "412 of 1,139", "saving…",
   * "after the archive". */
  readonly figure: string;
  /** 1 when done, 0 before it, the step's own while current (null when it
   * knows none). */
  readonly fraction: number | null;
  /** The current step's lines, drawn under it; empty for every other. */
  readonly lines: readonly string[];
}

export interface ImportSignAgent {
  /** The runtime, and its kind (rosetta's `claude-code`), which a host draws
   * its mark by. */
  readonly runtimeId: string;
  readonly kind: string;
  readonly name: string;
  /** Its place in the import (`queued` … `done`), or `live` for a runtime
   * this start watches without importing. */
  readonly state: RuntimeBackfill["state"] | "live";
  /** "2.10 GB processed", "Waiting to read", "Ready to browse". */
  readonly caption: string;
  /** "2.10 GB of 6.90 GB", "1.2 MB so far", "3,912 sessions"; empty when
   * there is nothing to count. */
  readonly figure: string;
  /** Its own meter: 1 done; null when it knows none. */
  readonly fraction: number | null;
  /** When it last brought something in ("3m ago"), once imported; null
   * before. */
  readonly lastNew: string | null;
}

export interface ImportSignSpace {
  /** What this start read from the agents ("8.40 GB", with "of 12.10 GB"
   * while every size is known and the import runs); null when it read
   * nothing (an index that started warm). */
  readonly read: string | null;
  /** What it keeps on disk: the four parts together. */
  readonly kept: string;
  /** The archive's share still in memory while the import builds it there;
   * null once it is on disk. */
  readonly inMemory: string | null;
  readonly parts: readonly ImportSignPart[];
  /** The index's own memory in use; null where the platform does not say. */
  readonly memory: string | null;
  /** When the parts were measured (Unix ms). */
  readonly measuredAt: number;
}

export interface ImportSignPart {
  readonly key: "record" | "archive" | "search" | "media";
  readonly name: string;
  readonly note: string;
  /** Its size as said ("3.70 GB"; "—" while it holds nothing). */
  readonly size: string;
  /** Its bytes on disk, and in memory (the archive's, while it is built
   * there), for a bar that draws the parts on one scale. */
  readonly disk: number;
  readonly memory: number;
}

const number = (value: number) => value.toLocaleString();
const plural = (n: number, noun: string) => `${noun}${n === 1 ? "" : "s"}`;

/** What each step opens, in the reader's terms. */
export const STEP_OPENS = [
  "Projects and sessions",
  "Open any conversation",
  "Charts and figures",
  "Search everything",
] as const;

/** "just now", "3m ago", "2h ago", "4d ago". */
export function agoText(ms: number | null | undefined, now: number): string | null {
  if (ms === null || ms === undefined) return null;
  const seconds = Math.max(0, Math.floor((now - ms) / 1_000));
  if (seconds < 60) return "just now";
  if (seconds < 3_600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3_600)}h ago`;
  return `${Math.floor(seconds / 86_400)}d ago`;
}

const asStep = (step: number): 1 | 2 | 3 | 4 => (step <= 1 ? 1 : step >= 4 ? 4 : step === 2 ? 2 : 3);

/** The step a paused or stopped run stands on: the card's run words say
 * where it stands, never which step, so read the step under them. */
function standingStep(feed: ActivityFeed, phase: ActivityPhase): 1 | 2 | 3 | 4 {
  const run = feed.ingest?.run;
  if (feed.ingest && run && (run.state === "paused" || run.state === "pausing" || run.state === "failed")) {
    const under = activityPhase({ ...feed, ingest: { ...feed.ingest, run: null } });
    return asStep(under?.step ?? phase.step);
  }
  return asStep(phase.step);
}

function agentFigure(row: RuntimeBackfill): string {
  switch (row.state) {
    case "queued": return row.sessions_total > 0 ? `${number(row.sessions_total)} ${plural(row.sessions_total, "session")}` : "";
    case "skimming": return row.files_total > 0 ? `${number(row.files_skimmed)} of ${number(row.files_total)} files` : "";
    case "streaming": return row.bytes_total > 0
      ? `${progressBytes(row.bytes_streamed)} of ${progressBytes(row.bytes_total)}`
      : `${progressBytes(row.bytes_streamed)} so far`;
    case "preparing":
    case "applying":
    case "done": return `${number(row.sessions_total)} ${plural(row.sessions_total, "session")}`;
  }
}

/** Each agent: the import's rows in serve order, then any runtime this start
 * watches that the import never listed (an index that started warm). */
function agentsOf(feed: ActivityFeed, now: number): ImportSignAgent[] {
  const restoring = feed.ingest?.phase === "restoring";
  const live = new Map<string, RuntimeIngestStatus>();
  for (const row of feed.live ?? []) live.set(row.runtime_id || row.kind, row);
  const agents: ImportSignAgent[] = (feed.ingest?.runtimes ?? []).map((row) => {
    const key = row.runtime_id || row.kind;
    const seen = live.get(key);
    live.delete(key);
    const status = runtimeActivity(row, restoring);
    return {
      runtimeId: row.runtime_id, kind: row.kind, name: runtimeName(row), state: row.state,
      caption: status.label, figure: agentFigure(row), fraction: status.fraction,
      lastNew: row.state === "done" ? agoText(seen?.last_activity_ms, now) : null,
    };
  });
  for (const row of live.values()) agents.push({
    runtimeId: row.runtime_id, kind: row.kind, name: runtimeName(row), state: "live",
    caption: "Ready to browse", figure: "", fraction: 1, lastNew: agoText(row.last_activity_ms, now),
  });
  return agents;
}

function spaceOf(feed: ActivityFeed, importing: boolean): ImportSignSpace | null {
  const storage = feed.storage;
  if (storage === null || storage === undefined) return null;
  const memory = feed.memory ?? null;
  const archiveInMemory = memory?.archive_bytes ?? null;
  const runtimes = feed.ingest?.runtimes ?? [];
  const read = runtimes.reduce((sum, row) => sum + row.bytes_streamed, 0);
  const sizes = readingSizes(runtimes);
  const whole = importing && sizes.unsized.length === 0 && sizes.total > 0;
  const ratio = storage.record_bytes > 0 ? read / storage.record_bytes : 0;
  const size = (bytes: number) => (bytes > 0 ? progressBytes(bytes) : "—");
  const parts: ImportSignPart[] = [
    {
      key: "record", name: "Record",
      note: ratio >= 1.1 ? `${ratio.toFixed(1)}× smaller than read · kept for good` : "kept for good",
      size: size(storage.record_bytes), disk: storage.record_bytes, memory: 0,
    },
    {
      key: "archive", name: "Archive",
      note: archiveInMemory === null ? "what you browse · rebuilt from the record"
        : storage.archive_bytes > 0 ? "being written to disk" : "in memory now · written to disk at step 3",
      size: size(storage.archive_bytes + (archiveInMemory ?? 0)),
      disk: storage.archive_bytes, memory: archiveInMemory ?? 0,
    },
    {
      key: "search", name: "Search",
      note: storage.search_bytes > 0 ? "rebuilt from the archive" : "built at step 4",
      size: size(storage.search_bytes), disk: storage.search_bytes, memory: 0,
    },
    {
      key: "media", name: "Media", note: "images and files agents sent",
      size: size(storage.media_bytes), disk: storage.media_bytes, memory: 0,
    },
  ];
  const kept = storage.record_bytes + storage.archive_bytes + storage.search_bytes + storage.media_bytes;
  return {
    read: read > 0 ? `${progressBytes(read)}${whole ? ` of ${progressBytes(sizes.total)}` : ""}` : null,
    kept: progressBytes(kept),
    inMemory: archiveInMemory === null ? null : progressBytes(archiveInMemory),
    parts,
    memory: memory === null ? null : progressBytes(memory.footprint_bytes),
    measuredAt: storage.measured_at,
  };
}

function stepFigure(n: number, state: ImportSignStep["state"], phase: ActivityPhase | null, feed: ActivityFeed): string {
  const runtimes = feed.ingest?.runtimes ?? [];
  const counted = readyConversations(feed);
  switch (n) {
    case 1: {
      if (state === "current") {
        const files = runtimes.reduce((sum, row) => sum + row.files_skimmed, 0);
        const total = runtimes.reduce((sum, row) => sum + row.files_total, 0);
        return total > 0 ? `${number(files)} of ${number(total)} files` : "finding…";
      }
      const sessions = runtimes.reduce((sum, row) => sum + row.sessions_total, 0);
      return sessions > 0 ? `${number(sessions)} ${plural(sessions, "session")}` : "found";
    }
    case 2:
      if (state === "later") return "after the scan";
      if (counted.total === 0) return state === "done" ? "readable" : phase?.amount ?? "reading…";
      return state === "done" && counted.readable >= counted.total
        ? `${number(counted.total)} readable` : `${number(counted.readable)} of ${number(counted.total)}`;
    case 3:
      if (state === "later") return "after reading";
      if (state === "done") return "saved";
      return phase?.key === "publishing" ? "saving…" : "finishing…";
    default: {
      if (state === "later") return "after the archive";
      if (state === "done") return "ready";
      const progress = feed.searchProgress;
      return progress && progress.documents_total > 0
        ? `${number(progress.documents_done)} of ${number(progress.documents_total)} messages` : "preparing…";
    }
  }
}

/** Reading's own line: what is read of the sizes known, and who is unsized. */
function readingLine(feed: ActivityFeed, phase: ActivityPhase): string {
  const sizes = readingSizes(feed.ingest?.runtimes ?? []);
  const unsized = sizes.unsized.length > 0 ? ` · ${unsizedText(sizes.unsized)}` : "";
  if (sizes.total === 0) return `${phase.amount.replace(/ · .*$/, "")}${unsized}`;
  const percent = phase.fraction === null ? "" : ` (${Math.floor(phase.fraction * 100)}%)`;
  return `${progressBytes(sizes.read)} of ${progressBytes(sizes.total)} read${percent}${unsized}`;
}

function stepsOf(
  step: 1 | 2 | 3 | 4, done: boolean, phase: ActivityPhase | null, feed: ActivityFeed,
  lines: readonly string[],
): ImportSignStep[] {
  return [1, 2, 3, 4].map((n) => {
    const state: ImportSignStep["state"] = done || n < step ? "done" : n === step ? "current" : "later";
    return {
      state, opens: STEP_OPENS[n - 1]!, figure: stepFigure(n, state, phase, feed),
      fraction: state === "done" ? 1 : state === "later" ? 0 : phase?.fraction ?? null,
      lines: state === "current" ? lines : [],
    };
  });
}

function eyebrowOf(tone: ImportSignTone, step: number, quiet: string): string {
  switch (tone) {
    case "rest": return "History · up to date";
    case "done": return "History · all set";
    case "attention": return `Needs attention · step ${step} of 4`;
    case "paused": return `${quiet} · step ${step} of 4`;
    case "working": return `Importing history · step ${step} of 4`;
  }
}

/**
 * The sign, from the room's status feed (with its readable conversations) and
 * the card's activity when the card would be up. Null while the index has not
 * answered and while it waits for consent: there is no import to sign yet.
 */
export function importSignOf(feed: ActivityFeed, activity: IngestActivity | null, now: number): ImportSign | null {
  if (feed.receivedAt === null || feed.ingest?.phase === "awaiting_consent") return null;
  const agents = agentsOf(feed, now);
  const disconnected = feed.connectionError !== null;
  if (activity === null) {
    // After the import (or an index that started warm): the sign stays, at
    // rest, and says what is kept and when each agent last brought anything.
    const lastNew = [...(feed.live ?? [])].reduce<number | null>(
      (latest, row) => (row.last_activity_ms != null && (latest === null || row.last_activity_ms > latest)
        ? row.last_activity_ms : latest), null);
    const tone: ImportSignTone = disconnected ? "paused" : "rest";
    const title = disconnected ? PAUSED_UPDATES.title : "Up to date";
    const detail = disconnected ? PAUSED_UPDATES.detail
      : agents.length > 0
        ? `Watching ${number(agents.length)} ${plural(agents.length, "agent")}. New conversations appear here as they happen.`
        : "New conversations appear here as they happen.";
    const eyebrow = disconnected ? "History · no updates" : eyebrowOf(tone, 4, "");
    const ago = agoText(lastNew, now);
    return {
      tone, step: 4, fraction: 1, eyebrow, title, count: null, detail, horizon: null, attention: null,
      label: `${eyebrow}: ${title}`, steps: stepsOf(4, true, null, feed, []), agents,
      space: spaceOf(feed, false),
      footer: disconnected ? "Reconnecting…"
        : feed.searchReadiness !== null && feed.searchReadiness !== "ready" ? "Search catching up"
        : ago === null ? "Watching for new conversations" : `Last new activity ${ago}`,
      elapsed: null,
    };
  }
  const { phase } = activity;
  const step = standingStep(feed, phase);
  const attention = phase.key === "failed" ? "import" : phase.key === "search_failed" ? "search" : null;
  const tone: ImportSignTone = disconnected ? "paused"
    : attention !== null ? "attention"
    : phase.key === "ready" ? "done"
    : phase.active ? "working" : "paused";
  const title = disconnected ? PAUSED_UPDATES.title : phase.title;
  const detail = disconnected ? PAUSED_UPDATES.detail : phase.detail;
  const count = disconnected || !phase.count ? null : phase.count.replace(/^ · /, "");
  const horizon = disconnected ? null : phase.horizon?.line ?? null;
  const quiet = disconnected ? "No updates" : "Paused";
  const eyebrow = eyebrowOf(tone, step, quiet);
  const lines = tone === "working" && phase.key === "reading"
    ? [readingLine(feed, phase), ...(horizon === null ? [] : [horizon])]
    : [detail];
  return {
    tone, step, fraction: tone === "done" ? 1 : phase.fraction, eyebrow, title, count, detail, horizon, attention,
    label: `${eyebrow}: ${title}${count === null ? "" : ` · ${count}`}`,
    steps: stepsOf(step, tone === "done", phase, feed, lines), agents,
    space: spaceOf(feed, tone !== "done"),
    footer: activity.footer, elapsed: activity.elapsed,
  };
}
