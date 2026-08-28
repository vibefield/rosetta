// The first run's honest room: the daemon is answering (status carries the
// meters) but the projection is still a private file, so there are no lists
// to veil yet. One centered panel, per-runtime meters, no fiction. The
// moment the projection publishes, meta arrives over the WS feed and the
// real room replaces this whole state.
import type { IngestProgress, RuntimeBackfill } from "@rosetta/protocol";

function formatBytes(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(0)} MB`;
  return `${Math.max(1, Math.round(bytes / 1e3))} KB`;
}

function runtimeCaption(runtime: RuntimeBackfill): string {
  switch (runtime.state) {
    case "queued":
      return `${runtime.files_total.toLocaleString()} files · queued`;
    case "skimming":
      return `census · ${runtime.files_skimmed.toLocaleString()} of ${runtime.files_total.toLocaleString()} files`;
    case "streaming":
      return `reading · ${formatBytes(runtime.bytes_streamed)} of ${formatBytes(
        runtime.bytes_total,
      )} · ${runtime.sessions_total.toLocaleString()} sessions`;
    case "done":
      return `done · ${runtime.sessions_total.toLocaleString()} sessions`;
  }
}

function runtimeProgress(runtime: RuntimeBackfill): number {
  switch (runtime.state) {
    case "queued":
      return 0;
    case "skimming":
      return runtime.files_total > 0
        ? runtime.files_skimmed / runtime.files_total
        : 0;
    case "streaming":
      return runtime.bytes_total > 0
        ? Math.min(1, runtime.bytes_streamed / runtime.bytes_total)
        : 0;
    case "done":
      return 1;
  }
}

export function ColdStartPanel({ ingest }: { readonly ingest: IngestProgress }) {
  // The empty-state shell, hand-laid: name first, promise second, and the
  // meters last — the row order a reader resolves this screen in.
  return (
    <div className="ah-empty" style={{ flex: 1 }}>
      <strong>Warming your archive</strong>
      <p>
        rosettad is reading every agent's store on this machine — the room fills
        in as it goes, newest work first. Everything stays on this machine.
      </p>
      <div className="ah-coldstart" role="status" aria-label="backfill progress">
        {ingest.runtimes.map((runtime) => (
          <div className="ah-coldstart__row" key={runtime.kind} data-state={runtime.state}>
            <code className="ah-coldstart__kind">{runtime.kind}</code>
            <div className="ah-meter">
              <i style={{ width: `${(runtimeProgress(runtime) * 100).toFixed(1)}%` }} />
            </div>
            <span className="ah-coldstart__caption">{runtimeCaption(runtime)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
