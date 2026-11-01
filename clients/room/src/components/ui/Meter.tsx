// The room's colourless meter. A real fraction fills it; an amount nobody
// knows sweeps instead (`data-indeterminate`, styled in ingest.css). A fixed
// sliver is a claim of progress that was never measured, so the room does
// not draw one: readiness is data, never a timer.

export interface MeterProps {
  /** 0..1 fills the meter (a sliver stays visible); null sweeps. */
  readonly fraction: number | null;
}

/** The width a known fraction draws: clamped, and never less than a sliver
 * once something is known, so a started step is never drawn as empty. */
export function meterWidth(fraction: number): string {
  return `${Math.min(100, Math.max(2, fraction * 100)).toFixed(1)}%`;
}

export function Meter({ fraction }: MeterProps) {
  return fraction === null ? (
    <div className="ah-meter" data-indeterminate="" aria-hidden="true">
      <i />
    </div>
  ) : (
    <div className="ah-meter" aria-hidden="true">
      <i style={{ width: meterWidth(fraction) }} />
    </div>
  );
}
