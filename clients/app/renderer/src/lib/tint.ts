// PROJECT IDENTITY — the colour is DEALT, not derived (design record §3).
// A project's hue is assigned the first time this room sees it, from the
// golden-ratio sequence over a 333° wheel (a 27° band around --vf-select is
// withheld), and the dealt number is stored so the colour never moves when
// projects are added, deleted, or renamed. The store is localStorage for now:
// the design record prices this honestly — the colour is a property of when
// the room first saw the project, so it does not travel between machines
// until the number does.

const TINT_H0 = 258;
const TINT_SKIP_AT = 238;
const TINT_SKIP_W = 27;
const TINT_SPAN = 360 - TINT_SKIP_W;
const PHI = 0.6180339887498949;

const STORE_KEY = "ah.tintSeq.v1";

function tintHue(seq: number): number {
  const pos = (TINT_H0 + seq * PHI * TINT_SPAN) % TINT_SPAN;
  return pos < TINT_SKIP_AT ? pos : pos + TINT_SKIP_W;
}

export function tintOf(seq: number): string {
  return `oklch(var(--tint-l) var(--tint-c) ${tintHue(seq).toFixed(1)})`;
}

type SeqMap = Record<string, number>;

function load(): SeqMap {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (raw) return JSON.parse(raw) as SeqMap;
  } catch {
    // a blocked or corrupt store deals fresh numbers this run
  }
  return {};
}

let seqs: SeqMap | null = null;

/** Deal (or recall) the stored sequence number for a project id. */
export function projectSeq(projectId: string): number {
  if (seqs === null) seqs = load();
  if (projectId in seqs) return seqs[projectId]!;
  const used = Object.values(seqs);
  const next = used.length === 0 ? 0 : Math.max(...used) + 1;
  seqs[projectId] = next;
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(seqs));
  } catch {
    // storage refused; the deal holds for this run
  }
  return next;
}

export function projectTint(projectId: string): string {
  return tintOf(projectSeq(projectId));
}
