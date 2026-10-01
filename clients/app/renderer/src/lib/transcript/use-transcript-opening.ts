import { useCallback, useLayoutEffect, useState } from "react";
import type { TimelineLayout } from "./use-transcript-tail";

/** The selected session owns this hook (its column is keyed by session id).
 * Mount and measure the real document under a stationary loading surface.
 * Readiness comes from the snapshot and initial team reads; two render frames
 * let preview measurement, disclosure layout and scroll placement finish.
 * There is no minimum spinner time and image downloads never block reading. */
export function useTranscriptOpening(snapshotReady: boolean, seeking: boolean) {
  const [layoutReady, setLayoutReady] = useState(false);
  const [revealed, setRevealed] = useState(false);
  const [readAvailable, setReadAvailable] = useState(false);
  const visible = !snapshotReady || !revealed;

  const onLayout = useCallback((layout: TimelineLayout) => {
    setLayoutReady(layout.ready && layout.initialReady !== false);
  }, []);
  const continueReading = useCallback(() => setReadAvailable(true), []);

  useLayoutEffect(() => {
    if (!snapshotReady) {
      setRevealed(false);
      setReadAvailable(false);
      return;
    }
    // Once readable, roster changes and ordinary reconnects stay inline.
    if (revealed || seeking || (!layoutReady && !readAvailable)) return;
    let frame = requestAnimationFrame(() => {
      frame = requestAnimationFrame(() => setRevealed(true));
    });
    return () => cancelAnimationFrame(frame);
  }, [snapshotReady, layoutReady, revealed, seeking, readAvailable]);

  return { visible, onLayout, continueReading, waitingForTeam: snapshotReady && !layoutReady };
}
