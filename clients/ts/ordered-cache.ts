/** Mutable internal ordering with immutable published arrays. Updates whose
 * sort key is unchanged replace one slot; moved/new rows use binary insertion. */
export class OrderedCache<T> {
  private readonly byId = new Map<string, T>();
  private ordered: T[] = [];
  private published: T[] | null = null;

  private readonly id: (row: T) => string;
  private readonly compare: (a: T, b: T) => number;
  constructor(id: (row: T) => string, compare: (a: T, b: T) => number) {
    this.id = id;
    this.compare = compare;
  }

  get(id: string): T | undefined {
    return this.byId.get(id);
  }

  private position(row: T): number {
    let low = 0;
    let high = this.ordered.length;
    while (low < high) {
      const mid = (low + high) >>> 1;
      if (this.compare(this.ordered[mid], row) < 0) low = mid + 1;
      else high = mid;
    }
    return low;
  }

  set(row: T): void {
    const id = this.id(row);
    const old = this.byId.get(id);
    if (old === row) return;
    this.published = null;
    if (old !== undefined) {
      const at = this.position(old);
      if (this.compare(old, row) === 0) {
        this.ordered[at] = row;
        this.byId.set(id, row);
        return;
      }
      this.ordered.splice(at, 1);
    }
    this.ordered.splice(this.position(row), 0, row);
    this.byId.set(id, row);
  }

  replace(rows: readonly T[]): void {
    this.byId.clear();
    for (const row of rows) this.byId.set(this.id(row), row);
    this.ordered = [...this.byId.values()].sort(this.compare);
    this.published = null;
  }

  values(): T[] {
    return (this.published ??= this.ordered.slice());
  }
}
