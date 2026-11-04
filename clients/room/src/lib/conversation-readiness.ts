// How many conversations a reader can open, counted the way the lists draw
// them: root sessions (top-level threads) from the pushed catalog. A root is
// readable once it is no longer Pending; a metadata-only placeholder (see
// `isMetadataOnly`) is never counted, since no import will reach it and a
// total holding it could never be reached. The count rides the same metadata
// delivery as the rows, so the card and the lists never disagree for a frame.
import type { SessionRow } from "@vibefield/rosetta-protocol";
import { isMetadataOnly } from "./import-state";
import { projectKey } from "./model";

export interface ConversationCount {
  readonly readable: number;
  readonly total: number;
}

/** Counted roots whose `updated_at` (source freshness, not activity) lies
 * within a span of the reader's clock. */
export interface ConversationHorizons {
  readonly day: ConversationCount;
  readonly week: ConversationCount;
}

export interface ConversationReadiness extends ConversationCount {
  /** Per project, keyed by `projectKey` (the unassigned bucket included). */
  readonly byProject: ReadonlyMap<string, ConversationCount>;
  readonly horizons: ConversationHorizons;
}

export const NO_CONVERSATIONS: ConversationReadiness = {
  readable: 0, total: 0, byProject: new Map(),
  horizons: { day: { readable: 0, total: 0 }, week: { readable: 0, total: 0 } },
};

const DAY_MS = 86_400_000;

interface Entry {
  row: SessionRow;
  pass: number;
  project: string;
  /** What this row adds to the counts now, so taking it back is exact. */
  counted: boolean;
  readable: boolean;
}

interface Tally { readable: number; total: number }

/**
 * The fold, kept across deliveries. The SDK keeps an unchanged row's object
 * identity, so a delivery costs one map lookup per unchanged row and real
 * work only for the rows that changed; the result is always what a full
 * count of the given rows would say, whatever order they come in (a render
 * that draws an older delivery again folds back to it).
 */
export class ConversationFold {
  private readonly entries = new Map<string, Entry>();
  /** Direct children per parent id: what tells a placeholder apart. */
  private readonly children = new Map<string, number>();
  private readonly projects = new Map<string, Tally>();
  /** Counted roots only, for the horizons. */
  private readonly roots = new Set<Entry>();
  private readable = 0;
  private total = 0;
  private pass = 0;
  private lastRows: readonly SessionRow[] | null = null;
  private lastNow: number | null = null;
  private last: ConversationReadiness = NO_CONVERSATIONS;
  private changed = true;

  read(rows: readonly SessionRow[], now: number): ConversationReadiness {
    if (rows !== this.lastRows) this.fold(rows);
    if (!this.changed && now === this.lastNow) return this.last;
    this.lastNow = now;
    this.changed = false;
    this.last = {
      readable: this.readable,
      total: this.total,
      byProject: new Map([...this.projects].map(([key, tally]) => [key, { ...tally }])),
      horizons: this.horizons(now),
    };
    return this.last;
  }

  private fold(rows: readonly SessionRow[]): void {
    this.lastRows = rows;
    const pass = ++this.pass;
    for (const row of rows) {
      const entry = this.entries.get(row.id);
      if (entry !== undefined && entry.row === row) {
        entry.pass = pass;
        continue;
      }
      this.changed = true;
      if (entry === undefined) {
        const added: Entry = { row, pass, project: projectKey(row), counted: false, readable: false };
        this.entries.set(row.id, added);
        this.settle(added);
        if (row.parent_session_id !== null) this.adopt(row.parent_session_id, 1);
        continue;
      }
      const parent = entry.row.parent_session_id;
      this.apply(entry, -1);
      entry.row = row;
      entry.pass = pass;
      entry.project = projectKey(row);
      this.settle(entry);
      if (parent !== row.parent_session_id) {
        if (parent !== null) this.adopt(parent, -1);
        if (row.parent_session_id !== null) this.adopt(row.parent_session_id, 1);
      }
    }
    if (this.entries.size === rows.length) return;
    // Rows the delivery no longer holds (a removal or a smaller snapshot).
    for (const [id, entry] of this.entries) {
      if (entry.pass === pass) continue;
      this.changed = true;
      this.apply(entry, -1);
      this.entries.delete(id);
      if (entry.row.parent_session_id !== null) this.adopt(entry.row.parent_session_id, -1);
    }
  }

  /** Recompute what an entry adds, from its row and its children, and add it. */
  private settle(entry: Entry): void {
    const row = entry.row;
    const root = row.parent_session_id === null;
    entry.counted = root && !isMetadataOnly(row, (this.children.get(row.id) ?? 0) > 0);
    entry.readable = entry.counted && row.stream_state !== "pending";
    this.apply(entry, 1);
  }

  private apply(entry: Entry, sign: 1 | -1): void {
    if (!entry.counted) return;
    const readable = entry.readable ? sign : 0;
    this.total += sign;
    this.readable += readable;
    const tally = this.projects.get(entry.project) ?? { readable: 0, total: 0 };
    tally.total += sign;
    tally.readable += readable;
    if (tally.total === 0) this.projects.delete(entry.project);
    else this.projects.set(entry.project, tally);
    if (sign === 1) this.roots.add(entry);
    else this.roots.delete(entry);
  }

  /** A child arrived under (or left) `parent`: whether the parent is a
   * placeholder can change only when it gains its first or loses its last. */
  private adopt(parent: string, delta: 1 | -1): void {
    const before = this.children.get(parent) ?? 0;
    const after = before + delta;
    if (after <= 0) this.children.delete(parent);
    else this.children.set(parent, after);
    if ((before > 0) === (after > 0)) return;
    const entry = this.entries.get(parent);
    if (entry === undefined) return;
    this.apply(entry, -1);
    this.settle(entry);
  }

  private horizons(now: number): ConversationHorizons {
    const day = { readable: 0, total: 0 };
    const week = { readable: 0, total: 0 };
    for (const entry of this.roots) {
      const age = now - entry.row.updated_at;
      if (age > 7 * DAY_MS) continue;
      week.total += 1;
      if (entry.readable) week.readable += 1;
      if (age > DAY_MS) continue;
      day.total += 1;
      if (entry.readable) day.readable += 1;
    }
    return { day, week };
  }
}
