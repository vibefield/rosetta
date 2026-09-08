// The roll call — consent decided in one screen, not a wizard. Discovery
// already ran (a read-only walk); the user arrives at its RESULTS: census
// rows, pre-checked where installed, one action. Nothing is read before
// Begin, and the reassurance line is the literal truth of the
// localhost-only daemon, stated exactly once, here.
import { useEffect, useMemo, useState } from "react";
import type { DiscoveryRow } from "@rosetta/protocol";
import { AgentMark } from "../lib/agents";
import { lastActive } from "../lib/format";
import { client } from "../lib/rosetta";

function formatBytes(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(0)} MB`;
  return `${Math.max(1, Math.round(bytes / 1e3))} KB`;
}

export function RollCallPanel() {
  const [rows, setRows] = useState<DiscoveryRow[] | null>(null);
  const [checked, setChecked] = useState<ReadonlySet<string>>(new Set());
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const controller = new AbortController();
    let timer: number | undefined;
    const load = () => {
      client
        .discoveries(controller.signal)
        .then((response) => {
          if (cancelled) return;
          setRows(response.discoveries);
          setChecked(
            new Set(
              response.discoveries
                .filter((row) => row.installed)
                .map((row) => row.kind),
            ),
          );
        })
        .catch(() => {
          if (!cancelled) timer = window.setTimeout(load, 5_000);
        });
    };
    load();
    return () => {
      cancelled = true;
      controller.abort();
      window.clearTimeout(timer);
    };
  }, []);

  const installed = useMemo(() => (rows ?? []).filter((row) => row.installed), [rows]);
  const absent = useMemo(() => (rows ?? []).filter((row) => !row.installed), [rows]);

  const begin = () => {
    setSubmitting(true);
    setError(null);
    client.enableRuntimes([...checked]).catch((err: unknown) => {
      setSubmitting(false);
      setError(err instanceof Error ? err.message : String(err));
    });
    // On success this panel simply waits: the daemon's phase flips to
    // backfilling and the room replaces the roll call on the next poll.
  };

  return (
    <div className="ah-empty" style={{ flex: 1 }}>
      <strong>Choose what rosetta reads</strong>
      <p>
        These agent stores were found on this machine. Nothing is read until
        you say so — and everything stays on this machine.
      </p>
      {rows === null ? (
        <div className="ah-meter">
          <i style={{ width: "8%" }} />
        </div>
      ) : (
        <>
          <div className="ah-rollcall" role="group" aria-label="Agents to enroll">
            {installed.map((row) => (
              <label className="ah-rollcall__row" key={row.kind}>
                <input
                  type="checkbox"
                  checked={checked.has(row.kind)}
                  disabled={submitting}
                  onChange={(event) => {
                    const next = new Set(checked);
                    if (event.target.checked) next.add(row.kind);
                    else next.delete(row.kind);
                    setChecked(next);
                  }}
                />
                <AgentMark kind={row.kind} />
                <b>{row.kind}</b>
                <span className="ah-rollcall__census">
                  <code>{row.data_root}</code> · {row.files.toLocaleString()}{" "}
                  {row.files === 1 ? "file" : "files"} · {formatBytes(row.bytes)}
                  {row.newest_modified_ms > 0 &&
                    ` · touched ${lastActive(row.newest_modified_ms, Date.now())}`}
                </span>
              </label>
            ))}
          </div>
          {absent.length > 0 && (
            <p className="ah-rollcall__absent">
              not found here: {absent.map((row) => row.kind).join(" · ")}
            </p>
          )}
          <button
            type="button"
            className="ah-rollcall__begin"
            disabled={checked.size === 0 || submitting}
            onClick={begin}
          >
            {submitting ? "Starting…" : "Begin ingest"}
          </button>
          {error !== null && <p className="ah-rollcall__error">{error}</p>}
        </>
      )}
    </div>
  );
}
