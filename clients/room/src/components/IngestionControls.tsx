import { useEffect, useRef, useState } from "react";
import { ApiRequestError } from "@vibefield/rosetta-protocol";
import type { Ingestion, IngestionAction, IngestionCommand } from "@vibefield/rosetta-protocol";
import { useRoom } from "../room-context";

interface Intent {
  id: string;
  action: IngestionAction;
  command: IngestionCommand;
}

/** A host may supply observation without mutation authority. Pending intent
 * keeps its key, revision and index across uncertain transport outcomes. */
export function IngestionControls({ run }: { readonly run: Ingestion | null | undefined }) {
  const { ingestion } = useRoom();
  const [intent, setIntent] = useState<Intent | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef<AbortController | null>(null);
  useEffect(() => {
    setIntent(null); setBusy(false); setError(null);
    return () => {
      pending.current?.abort();
      pending.current = null;
    };
  }, [run?.index_id, run?.id, ingestion]);
  useEffect(() => {
    if (!intent || !run || run.revision <= intent.command.expected_revision) return;
    const settled = intent.action === "pause" ? run.state === "paused" || run.state === "pausing" : run.state === "running";
    if (settled) {
      pending.current?.abort();
      pending.current = null;
      setIntent(null); setBusy(false); setError(null);
    }
  }, [intent, run]);
  if (!ingestion || !run) return null;
  const action = intent?.action ?? run.allowed_actions[0];
  if (!action) return error ? <p role="alert">{error}</p> : null;
  const submit = async () => {
    if (pending.current) return;
    const next = intent ?? { id: run.id, action, command: {
      indexId: run.index_id, expected_revision: run.revision, idempotencyKey: crypto.randomUUID(),
    } };
    const request = new AbortController();
    pending.current = request;
    setIntent(next); setBusy(true); setError(null);
    try {
      await ingestion[next.action](next.id, next.command, request.signal);
      // The ordinary status poll supplies the observed state and progress.
      if (pending.current === request) setIntent(null);
    } catch (cause) {
      if (pending.current !== request) return;
      if (cause instanceof ApiRequestError && cause.status >= 400 && cause.status < 500) {
        setIntent(null);
        setError("Import controls changed. Check the current state and try again.");
      } else {
        setError("The service did not confirm this request. Retry to check its outcome.");
      }
    } finally {
      if (pending.current === request) { pending.current = null; setBusy(false); }
    }
  };
  return <div className="ah-ingest-controls">
    <button type="button" disabled={busy} onClick={() => { void submit(); }}>
      {busy ? "Sending…" : `${intent && error ? "Retry " : ""}${action === "pause" ? "Pause import" : "Resume import"}`}
    </button>
    {error && <p role="alert">{error}</p>}
  </div>;
}
