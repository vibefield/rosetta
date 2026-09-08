/** Ordinals belong to sessions, not the merged team chronology. New sources
 * start with a baseline; history, patches and replay cannot inflate the count. */
export interface TailRow {
  readonly session_id: string;
  readonly timeline_id: string;
  readonly ordinal: number;
}

export class TailArrivals {
  private maxima = new Map<string, number>();

  update<T extends TailRow>(rows: readonly T[]): T[] {
    const fresh: T[] = [];
    const next = new Map(this.maxima);
    for (const row of rows) {
      const previous = this.maxima.get(row.session_id);
      if (previous !== undefined && row.ordinal > previous) fresh.push(row);
      next.set(row.session_id, Math.max(next.get(row.session_id) ?? -1, row.ordinal));
    }
    this.maxima = next;
    return fresh;
  }

  /** Even an empty initial snapshot establishes this session's live tail. */
  prime(sessionId: string): void {
    if (!this.maxima.has(sessionId)) this.maxima.set(sessionId, -1);
  }

  reset(): void {
    this.maxima.clear();
  }
}

/** Time-based easing stays equally smooth on 60 Hz and 120 Hz displays.
 * Retargeting never restarts a browser smooth-scroll on each streamed token. */
export function tailStep(current: number, target: number, elapsed: number): number {
  if (Math.abs(target - current) <= 0.5) return target;
  return current + (target - current) * (1 - Math.exp(-Math.min(elapsed, 64) / 70));
}
