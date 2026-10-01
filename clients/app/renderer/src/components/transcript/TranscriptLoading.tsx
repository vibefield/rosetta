import "../../styles/transcript-loading.css";

export function TranscriptLoading({ error, waitingForTeam, seeking, canContinue, indexing, onRetry, onContinue }: {
  readonly error: string | null;
  readonly waitingForTeam: boolean;
  readonly seeking: boolean;
  readonly canContinue: boolean;
  readonly indexing: boolean;
  readonly onRetry: () => void;
  readonly onContinue: () => void;
}) {
  const label = error !== null ? "Couldn’t load messages"
    : waitingForTeam ? "Loading team messages…"
    : seeking ? "Finding message…"
    : indexing ? "Preparing messages…" : "Loading messages…";
  return <div className="tr-loading" data-error={error !== null || undefined}>
    <div className="tr-loading__body">
      <div className="tr-loading__sketch" aria-hidden="true">
        <div className="tr-loading__prompt"><i /><i /></div>
        <div className="tr-loading__reply">
          <div className="tr-loading__byline"><b /><i /></div>
          <i /><i /><i />
        </div>
        <div className="tr-loading__fold"><b>›</b><i /></div>
      </div>
      <div className="tr-loading__status" role="status" aria-live="polite" aria-atomic="true">
        {error === null && <div className="tr-loading__meter" aria-hidden="true"><i /></div>}
        <strong>{label}</strong>
        {error !== null && <p>{error}</p>}
      </div>
      {error !== null ? <button type="button" onClick={onRetry}>Try again</button>
        : canContinue && <button type="button" onClick={onContinue}>Read available messages</button>}
    </div>
  </div>;
}
