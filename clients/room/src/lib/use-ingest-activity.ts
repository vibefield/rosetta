import { useEffect, useMemo, useRef, useState } from "react";
import { activityIdentity, activityPhase, progressBytes, progressDuration, resumedRun, sampleActivity } from "./ingest-activity";
import type { ActivityFeed, ActivitySamples, IngestActivity } from "./ingest-activity";
import type { ConversationReadiness } from "./conversation-readiness";

/** `conversations` is the room's fold of the catalog it draws the rows from,
 * so the card's counts change in the same render as the rows. */
export function useIngestActivity(given: ActivityFeed, conversations: ConversationReadiness | null = null): IngestActivity | null {
  // A resumed build is known by the restoring phase it passed through; the
  // mark outlives that phase until the import goes live.
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
  const sawWork = useRef(false);
  const [showCompletion, setShowCompletion] = useState(false);
  const key = phase?.key;
  useEffect(() => {
    const current = activityPhase(polled);
    if (current) setSamples((previous) => sampleActivity(previous, polled, current));
    setNow(Date.now());
  }, [polled]);
  useEffect(() => {
    if (key && key !== "ready" && key !== "paused") sawWork.current = true;
    if (key !== "ready") return;
    setShowCompletion(sawWork.current);
    const timer = window.setTimeout(() => { setShowCompletion(false); sawWork.current = false; }, 5_000);
    return () => window.clearTimeout(timer);
  }, [key]);
  useEffect(() => {
    if (!key || (key === "ready" && !showCompletion)) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [key, showCompletion]);
  if (!phase || (key === "ready" && !showCompletion)) return null;
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
  return { phase, feed, footer, elapsed };
}
