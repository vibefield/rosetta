// PROJECT IDENTITY — the colour is DEALT, not derived (design record §3).
// A project's hue is assigned the first time this room sees it, from the
// golden-ratio sequence over a 333° wheel (a 27° band around --vf-select is
// withheld), and the dealt number is stored so the colour never moves when
// projects are added, deleted, or renamed. The store is the room's storage:
// the design record prices this honestly — the colour is a property of when
// the room first saw the project, so it does not travel between machines
// until the number does.

import type { KeyValueStore } from "./room-storage";

const TINT_H0 = 258;
const TINT_SKIP_AT = 238;
const TINT_SKIP_W = 27;
const TINT_SPAN = 360 - TINT_SKIP_W;
const PHI = 0.6180339887498949;

/** Under the room's storage prefix, versioned: the dealt numbers by project. */
const STORE_KEY = "tintSeq.v1";

function tintHue(seq: number): number {
  const pos = (TINT_H0 + seq * PHI * TINT_SPAN) % TINT_SPAN;
  return pos < TINT_SKIP_AT ? pos : pos + TINT_SKIP_W;
}

export function tintOf(seq: number): string {
  return `oklch(var(--tint-l) var(--tint-c) ${tintHue(seq).toFixed(1)})`;
}

type SeqMap = Record<string, number>;

/** One room's dealer: its stored numbers, dealt once each. */
export interface TintDealer {
  /** Deal (or recall) the stored sequence number for a project id. */
  projectSeq(projectId: string): number;
  projectTint(projectId: string): string;
}

export function createTintDealer(store: KeyValueStore | null): TintDealer {
  let seqs: SeqMap | null = null;

  function load(): SeqMap {
    try {
      const raw = store?.getItem(STORE_KEY) ?? null;
      if (raw) return JSON.parse(raw) as SeqMap;
    } catch {
      // a blocked or corrupt store deals fresh numbers this run
    }
    return {};
  }

  function projectSeq(projectId: string): number {
    if (seqs === null) seqs = load();
    if (projectId in seqs) return seqs[projectId]!;
    const used = Object.values(seqs);
    const next = used.length === 0 ? 0 : Math.max(...used) + 1;
    seqs[projectId] = next;
    try {
      store?.setItem(STORE_KEY, JSON.stringify(seqs));
    } catch {
      // storage refused; the deal holds for this run
    }
    return next;
  }

  return { projectSeq, projectTint: (projectId) => tintOf(projectSeq(projectId)) };
}
