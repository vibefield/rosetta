import { useEffect, useMemo, useRef } from "react";
import type { RoomImportSign } from "../host-inputs";
import type { ConversationReadiness } from "./conversation-readiness";
import type { DaemonStatusFeed } from "./hooks";
import { importSignOf } from "./import-sign";
import type { IngestActivity } from "./ingest-activity";

/**
 * R11 — hand the host the import's sign whenever its words or figures change,
 * and null as the room goes. Only a room whose host draws the sign reads it.
 * The host's `importSign` should be one object for the room's life: a new one
 * is a new host, told null and then the sign again.
 */
export function useImportSign(
  host: RoomImportSign | undefined,
  status: DaemonStatusFeed,
  conversations: ConversationReadiness | null,
  activity: IngestActivity | null,
): void {
  const sign = useMemo(
    () => (host === undefined ? null : importSignOf({ ...status, conversations }, activity, Date.now())),
    [host, status, conversations, activity],
  );
  // The activity's clock moves every second; the host hears only a change.
  const content = sign === null ? null : JSON.stringify(sign);
  const latest = useRef(sign);
  latest.current = sign;
  const onSign = host?.onSign;
  useEffect(() => {
    onSign?.(latest.current);
  }, [onSign, content]);
  useEffect(() => (onSign === undefined ? undefined : () => onSign(null)), [onSign]);
}
