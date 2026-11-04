import { useEffect, useMemo, useState } from "react";
import { activityIdentity, activityPhase, progressBytes, progressDuration, resumedRun, sampleActivity } from "./ingest-activity";
import type { ActivityFeed, ActivitySamples, IngestActivity } from "./ingest-activity";
import type { ConversationReadiness } from "./conversation-readiness";

/** How long "Search ready" stays, and how long its fold takes (ingest.css,
 * `.ah-ingest-fold`), before the card goes. */
const COMPLETION_MS = 5_000;
export const LEAVING_MS = 360;

/** `conversations` is the room's fold of the catalog it draws the rows from,
 * so the card's counts change in the same render as the rows. */
export function useIngestActivity(given: ActivityFeed, conversations: ConversationReadiness | null = null): IngestActivity | null {
  // A resumed build is known by its runtimes' course, or, from a daemon that
  // names none, by the restoring phase it passed through; the mark outlives
  // that phase until the import goes live.
  const [resumedKey, setResumedKey] = useState<string | null>(null);
  const nextResumedKey = resumedRun(resumedKey, given);
  if (nextResumedKey !== resumedKey) setResumedKey(nextResumedKey);
  const resumed = nextResumedKey !== null;
  const polled = useMemo(() => (resumed ? { ...given, resumed } : given), [given, resumed]);
  // The counts move with every metadata delivery; the samples and the clock
  // follow the status poll alone, so a delivery costs the room no second render.
  const feed = useMemo(() => (conversations === null ? polled : { ...polled, conversations }), [polled, conversations]);
  const phase = activityPhase(feed);
  const [samples, setSamples] = useState<ActivitySamples | null>(null);
  const [now, setNow] = useState(Date.now);
  // The end: "Search ready" shows on one line for five seconds, then the
  // card folds its height away (`leaving`), then it goes. Only a run seen
  // working ends this way; a room opened on a finished archive shows none.
  const [sawWork, setSawWork] = useState(false);
  const [end, setEnd] = useState<"shown" | "leaving" | "gone" | null>(null);
  const key = phase?.key;
  if (key !== undefined && key !== "ready" && key !== "paused") {
    if (!sawWork) setSawWork(true);
    if (end !== null) setEnd(null);
  }
  // Decided in the render that first draws "ready", so the card never
  // blinks out for a frame on its way there.
  if (key === "ready" && end === null) setEnd(sawWork ? "shown" : "gone");
  useEffect(() => {
    const current = activityPhase(polled);
    if (current) setSamples((previous) => sampleActivity(previous, polled, current));
    setNow(Date.now());
  }, [polled]);
  useEffect(() => {
    if (end !== "shown" && end !== "leaving") return;
    const timer = window.setTimeout(() => {
      if (end === "shown") setEnd("leaving");
      else { setEnd("gone"); setSawWork(false); }
    }, end === "shown" ? COMPLETION_MS : LEAVING_MS);
    return () => window.clearTimeout(timer);
  }, [end]);
  const finished = key === "ready" && end !== "shown" && end !== "leaving";
  useEffect(() => {
    if (!key || finished) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [key, finished]);
  if (!phase || finished) return null;
  const sinceAnswer = Math.max(0, now - (feed.receivedAt ?? now));
  const elapsed = `${progressDuration(feed.uptimeMs + (phase.active ? sinceAnswer : 0))} elapsed`;
  const sinceChange = Math.max(0, now - (samples?.changedAt ?? now));
  const rate = sinceChange < 4_000 && samples?.identity === activityIdentity(feed, phase)
    ? samples.rate : null;
  let footer = "Connected";
  if (feed.connectionError) footer = `Last update ${progressDuration(sinceAnswer)} ago`;
  else if (rate && phase.unit) footer = phase.unit === "bytes"
    ? `${progressBytes(rate)}/s` : `${Math.round(rate).toLocaleString()} ${phase.unit === "documents" ? "messages" : phase.unit === "records" ? "items" : phase.unit}/s`;
  else if (phase.unit && sinceChange >= 10_000) footer = `Connected · No new counts for ${progressDuration(sinceChange)}`;
  else if (!phase.active) footer = key === "ready" ? "Complete" : key === "paused" ? "Paused" : key === "failed" ? "Stopped" : "Ready to browse";
  return { phase, feed, footer, elapsed, leaving: key === "ready" && end === "leaving" };
}
