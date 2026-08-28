// The veil — what pending looks like. A section that cannot yet vouch for
// its content frosts over while the content keeps rendering and updating
// beneath: the motion under the frost IS the progress animation, and the
// meter is just the caption. Laws (design record, "The Cold Start"):
// readiness is data, never a timer; only the ready may claim emptiness; a
// veil overlays and never changes geometry (H-2); and a veil never descends
// on a scope that has already been ready.

export interface VeilProps {
  /** One label line, e.g. "preparing · 2,314 of 5,912 sessions". */
  readonly label: string;
  /** 0..1 fills the meter; null renders the meter idling at a sliver. */
  readonly progress: number | null;
}

export function Veil({ label, progress }: VeilProps) {
  const width =
    progress === null ? "8%" : `${Math.min(100, Math.max(2, progress * 100)).toFixed(1)}%`;
  return (
    <div className="ah-veil" role="status" aria-label={label}>
      <div className="ah-meter">
        <i style={{ width }} />
      </div>
      <span className="ah-veil__label">{label}</span>
    </div>
  );
}
