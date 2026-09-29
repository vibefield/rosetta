// §08 — the head of the document, as pure state.
//
// The foot's rule is "a notice, never a jump". The head is its mirror, and
// Q7 B (decided 2026-09-02) says how: within one viewport of the top the page
// above is FETCHED and HELD, never inserted; 120px of upward travel at
// scrollTop 0 spends the bar and reveals it. Everything here is arithmetic —
// no React, no DOM, no clock of its own — so the transitions can be proven in
// `test/transcript-head.test.mjs` and the hook that owns the listeners stays
// thin.
import type { TimelineEntry } from "@rosetta/protocol";

/** One backward page. The daemon's limit and the record's figure. */
export const EARLIER_PAGE_SIZE = 200;
/** "about two notches or one short flick" — §08. */
export const PULL_THRESHOLD_PX = 120;
/** The accumulator decays after half a second of stillness (§08). */
export const PULL_DECAY_MS = 500;
/** The receipt's dwell. The record's `land()` clears the pill at 1800ms. */
export const RECEIPT_MS = 1_800;
/** `--frost` rises 0 → 1 over the first 24px of scroll (§08, the top fade). */
export const FROST_RANGE_PX = 24;

/** Muted, never red — nothing was destroyed. The retry lives in the line. */
export const EARLIER_FAILED_MESSAGE = "the daemon did not answer";
/** The protocol's 410, drawn on the data gap's dashed line. */
export const EARLIER_REWRITTEN_MESSAGE = "rewritten while you read · reloading from the end";

/** What the head line offers the reader right now. */
export type EarlierState =
  /** Nothing above: the cursor reached the top, or the session is short. */
  | { readonly kind: "none" }
  /** History above, nothing fetched. Approaching the top starts a fetch. */
  | { readonly kind: "idle" }
  /** A page is in flight and will be held, not inserted. */
  | { readonly kind: "fetching" }
  /** A page is fetched and waiting beside the document. Weight = readiness. */
  | { readonly kind: "held"; readonly count: number }
  /** The pull arrived before the page did; it lands on arrival. */
  | { readonly kind: "revealing" }
  /** The daemon did not answer. `retryEarlier()` tries again. */
  | { readonly kind: "failed"; readonly message: string };

/** The 410 sentence, surfaced once after the paged history is dropped. */
export interface EarlierNotice {
  readonly kind: "rewritten";
  readonly message: string;
  /** Increments per notice so an effect can key on it. */
  readonly seq: number;
}

/** The last landing — the receipt pill's figure and the scroll adjustment's key. */
export interface EarlierLanding {
  /** Rows the document actually gained, deduplicated against what it had. */
  readonly count: number;
  /** Increments on every landing, including a landing of zero fresh rows. */
  readonly seq: number;
}

export interface EarlierModel {
  /** Page identities for reveal receipts. The subscription owns row values. */
  readonly older: readonly TimelineEntry[];
  /** `undefined` = untried · `null` = top of history · string = next page back. */
  readonly cursor: string | null | undefined;
  /** The fetched page waiting beside the document, and the cursor it carried. */
  readonly held: readonly TimelineEntry[] | null;
  readonly heldCursor: string | null | undefined;
  /** Rows the held page would actually add — never the raw page length. */
  readonly heldCount: number;
  readonly phase: "rest" | "fetching" | "revealing" | "failed";
  readonly failure: string | null;
  readonly notice: EarlierNotice | null;
  readonly landing: EarlierLanding | null;
  /** Stamps the fetch in flight; a stale answer is dropped, never applied. */
  readonly fetchSeq: number;
  readonly landSeq: number;
  readonly noticeSeq: number;
}

export function initialEarlierModel(): EarlierModel {
  return {
    older: [],
    cursor: undefined,
    held: null,
    heldCursor: undefined,
    heldCount: 0,
    phase: "rest",
    failure: null,
    notice: null,
    landing: null,
    fetchSeq: 0,
    landSeq: 0,
    noticeSeq: 0,
  };
}

/** What the reducer needs from outside itself. */
export interface EarlierContext {
  /** The live tail's ids. A page that repeats them added nothing. */
  readonly tailIds: ReadonlySet<string>;
  readonly pageSize: number;
}

export type EarlierAction =
  /** The session changed: drop the paged history AND the held page. */
  | { readonly type: "reset" }
  | { readonly type: "snapshot"; readonly cursor: string | null }
  /** Fetch the page above and keep it aside. No-op unless idle or failed. */
  | { readonly type: "hold" }
  /** Spend the bar. Lands a held page now, or marks a fetch to land on arrival. */
  | { readonly type: "reveal" }
  /** A page came back. Dropped unless `seq` still stamps the fetch in flight. */
  | {
      readonly type: "page";
      readonly seq: number;
      readonly entries: readonly TimelineEntry[];
      readonly cursor: string | null;
    }
  /** The daemon did not answer. */
  | { readonly type: "fail"; readonly seq: number; readonly message: string }
  /** The projection was rewritten underneath (HTTP 410). */
  | { readonly type: "expired"; readonly seq: number }
  /** Dismiss the 410 sentence once it has been read. */
  | { readonly type: "clearNotice" };

/** True when there is provably history above what is loaded. */
function hasHistoryAbove(model: EarlierModel, ctx: EarlierContext): boolean {
  // An untried snapshot shorter than one page IS the whole session.
  if (model.cursor === undefined) return ctx.tailIds.size >= ctx.pageSize;
  return model.cursor !== null;
}

/** The public state, derived — never stored twice. */
export function earlierState(model: EarlierModel, ctx: EarlierContext): EarlierState {
  if (model.phase === "failed") {
    return { kind: "failed", message: model.failure ?? EARLIER_FAILED_MESSAGE };
  }
  if (model.phase === "revealing") return { kind: "revealing" };
  if (model.phase === "fetching") return { kind: "fetching" };
  if (model.held !== null) return { kind: "held", count: model.heldCount };
  return hasHistoryAbove(model, ctx) ? { kind: "idle" } : { kind: "none" };
}

/** Rows the page adds that the document does not already have. */
function freshCount(
  page: readonly TimelineEntry[],
  older: readonly TimelineEntry[],
  ctx: EarlierContext,
): number {
  const seen = new Set(ctx.tailIds);
  for (const row of older) seen.add(row.timeline_id);
  let fresh = 0;
  for (const row of page) {
    if (seen.has(row.timeline_id)) continue;
    seen.add(row.timeline_id);
    fresh += 1;
  }
  return fresh;
}

/** Prepend a held page and record the receipt. Landing never moves the reader —
 *  the hook adjusts scrollTop by the first old row's measured delta. */
function land(
  model: EarlierModel,
  page: readonly TimelineEntry[],
  cursor: string | null | undefined,
  ctx: EarlierContext,
): EarlierModel {
  const seq = model.landSeq + 1;
  return {
    ...model,
    older: [...page, ...model.older],
    cursor: cursor,
    held: null,
    heldCursor: undefined,
    heldCount: 0,
    phase: "rest",
    failure: null,
    // The daemon answered: the 410 sentence has done its work.
    notice: null,
    landing: { count: freshCount(page, model.older, ctx), seq },
    landSeq: seq,
  };
}

export function reduceEarlier(
  model: EarlierModel,
  action: EarlierAction,
  ctx: EarlierContext,
): EarlierModel {
  switch (action.type) {
    case "snapshot": return { ...model, cursor: action.cursor };
    case "reset": {
      // A held page belongs to the session it was fetched for. It never
      // survives the switch, and neither does a stale 410 sentence. Bumping
      // the stamp orphans any answer still in flight.
      const pristine =
        model.older.length === 0 &&
        model.cursor === undefined &&
        model.held === null &&
        model.phase === "rest" &&
        model.notice === null &&
        model.landing === null;
      if (pristine) return model;
      return { ...initialEarlierModel(), fetchSeq: model.fetchSeq + 1 };
    }

    case "hold": {
      if (model.phase === "fetching" || model.phase === "revealing") return model;
      if (model.held !== null) return model;
      if (!hasHistoryAbove(model, ctx)) return model;
      return {
        ...model,
        phase: "fetching",
        failure: null,
        fetchSeq: model.fetchSeq + 1,
      };
    }

    case "reveal": {
      if (model.held !== null) return land(model, model.held, model.heldCursor, ctx);
      // A second pull while one is in flight is ignored, not queued (§08).
      if (model.phase === "fetching") return { ...model, phase: "revealing" };
      return model;
    }

    case "page": {
      if (action.seq !== model.fetchSeq) return model;
      // The reader already asked: land it the instant it arrives.
      if (model.phase === "revealing") {
        return land(model, action.entries, action.cursor, ctx);
      }
      const fresh = freshCount(action.entries, model.older, ctx);
      if (fresh === 0) {
        // A page that adds nothing is not a page. The head page is exactly
        // this: it repeats the subscription's snapshot to mint a cursor.
        // Settle the cursor and stand down rather than hold empty weight.
        return { ...model, phase: "rest", failure: null, notice: null, cursor: action.cursor };
      }
      return {
        ...model,
        phase: "rest",
        failure: null,
        // The daemon answered: the 410 sentence has done its work.
        notice: null,
        held: action.entries,
        heldCursor: action.cursor,
        heldCount: fresh,
      };
    }

    case "fail": {
      if (action.seq !== model.fetchSeq) return model;
      return { ...model, phase: "failed", failure: action.message };
    }

    case "expired": {
      if (action.seq !== model.fetchSeq) return model;
      // The projection rewrote underneath the reader: drop the paged history
      // and the held page, start again from the head, and say so once.
      const seq = model.noticeSeq + 1;
      return {
        ...initialEarlierModel(),
        fetchSeq: model.fetchSeq + 1,
        notice: { kind: "rewritten", message: EARLIER_REWRITTEN_MESSAGE, seq },
        noticeSeq: seq,
      };
    }

    case "clearNotice":
      return model.notice === null ? model : { ...model, notice: null };
  }
}

/** Rows still above the loaded document, when the session's total is known.
 *  Returns null when it is not — §08: "when it is not, the word stands alone." */
export function earlierAboveOf(total: number | null, loaded: number): number | null {
  if (total === null || !Number.isFinite(total)) return null;
  return Math.max(0, Math.trunc(total) - loaded);
}

// ── the pull accumulator ────────────────────────────────────────────────────
// Upward travel at scrollTop 0 accumulates; 120px fires. Downward travel or
// half a second of stillness resets it, so drifting to the top never reveals
// and a deliberate push always does.

export interface PullModel {
  /** Pixels of upward travel banked, clamped to the threshold. */
  readonly travel: number;
  /** When the last travel landed, for the decay. */
  readonly at: number;
}

export const initialPull: PullModel = { travel: 0, at: 0 };

export interface PullEvent {
  /** Positive = toward earlier (wheel up, finger down). Negative = away. */
  readonly travel: number;
  readonly atTop: boolean;
  readonly now: number;
}

export interface PullOptions {
  readonly threshold?: number;
  readonly decayMs?: number;
}

export interface PullResult {
  readonly model: PullModel;
  /** True on the frame the accumulator crosses the threshold. */
  readonly fire: boolean;
}

export function advancePull(
  model: PullModel,
  event: PullEvent,
  options: PullOptions = {},
): PullResult {
  const threshold = options.threshold ?? PULL_THRESHOLD_PX;
  const decayMs = options.decayMs ?? PULL_DECAY_MS;
  // Leaving the top spends nothing and banks nothing.
  if (!event.atTop) return { model: initialPull, fire: false };
  // Any downward travel resets — a flick past the top is not a pull.
  if (event.travel <= 0) return { model: initialPull, fire: false };
  const decayed = event.now - model.at >= decayMs ? 0 : model.travel;
  const travel = Math.min(threshold, decayed + event.travel);
  if (travel >= threshold) return { model: initialPull, fire: true };
  return { model: { travel, at: event.now }, fire: false };
}

/** The bar's own reading: 0 at rest, 1 the instant before it fires. */
export function pullProgress(model: PullModel, threshold = PULL_THRESHOLD_PX): number {
  if (threshold <= 0) return 0;
  return Math.min(1, Math.max(0, model.travel / threshold));
}

/** `--frost`: 0 at rest, 1 by 24px of scroll. */
export function frostOf(scrollTop: number, range = FROST_RANGE_PX): number {
  if (range <= 0) return scrollTop > 0 ? 1 : 0;
  return Math.min(1, Math.max(0, scrollTop / range));
}

// ── the head line ───────────────────────────────────────────────────────────
// The marker family's bare-text form plus one instrument: the veil's
// colourless bar. `state` is the record's own `data-state` value.

export type HeadState =
  | "rest"
  | "armed"
  | "fetching"
  | "held"
  | "loading"
  | "finding"
  | "start"
  | "error"
  | "rewritten";

export interface HeadView {
  readonly state: HeadState;
  /** Text before the figure. */
  readonly lead: string;
  /** The bold segment, or null when the line carries no figure. */
  readonly figure: string | null;
  /** Text after the figure. */
  readonly trail: string;
  /** Draw the retry act inside the line — a text act, never a filled button. */
  readonly retry: boolean;
  readonly bar: boolean;
  /** Inline width for the fill, or null to let the stylesheet drive it. */
  readonly fill: string | null;
}

export interface HeadInput {
  readonly earlier: EarlierState;
  /** Rows still above, or null when the session's total is unknown. */
  readonly above: number | null;
  /** The accumulator, 0..1. */
  readonly pull: number;
  /** The clock an anchor search is paging toward, when one is running. */
  readonly finding?: string | null;
  /** `Mon 31 Aug · 23:18:12` — the session's first second, already worded. */
  readonly startLabel?: string | null;
  readonly pageSize?: number;
}

function figureOf(above: number | null): string {
  return above === null ? "earlier" : `${above} earlier`;
}

export function describeHead(input: HeadInput): HeadView {
  const { earlier, above } = input;
  const pull = Math.min(1, Math.max(0, input.pull));
  const pageSize = input.pageSize;
  const finding = input.finding ?? null;
  const startLabel = input.startLabel ?? null;

  // Loading with a reason: a moving document always states its cause.
  if (finding !== null) {
    return {
      state: "finding",
      lead: "finding",
      figure: finding,
      trail: "· loading earlier",
      retry: false,
      bar: true,
      fill: null,
    };
  }
  if (earlier.kind === "failed") {
    return {
      state: "error",
      lead: `${earlier.message} ·`,
      figure: null,
      trail: "",
      retry: true,
      bar: false,
      fill: null,
    };
  }
  // The start is a fact, drawn — and a pull here does nothing.
  if (earlier.kind === "none") {
    return {
      state: "start",
      lead: startLabel === null ? "session start" : "session start ·",
      figure: startLabel,
      trail: "",
      retry: false,
      bar: false,
      fill: null,
    };
  }
  if (earlier.kind === "revealing") {
    return {
      state: "loading",
      lead: "loading",
      figure: pageSize === undefined ? "earlier" : `${pageSize} earlier`,
      trail: "",
      retry: false,
      bar: true,
      fill: null,
    };
  }
  if (pull > 0) {
    // Held weight is spent by the pull; unheld weight is gathered by it.
    const held = earlier.kind === "held";
    const percent = Math.round(held ? 100 - 100 * pull : 100 * pull);
    return {
      state: "armed",
      lead: "",
      figure: figureOf(above),
      trail: "· keep going",
      retry: false,
      bar: true,
      fill: `${percent}%`,
    };
  }
  if (earlier.kind === "fetching") {
    return {
      state: "fetching",
      lead: "",
      figure: figureOf(above),
      trail: "· pull to load",
      retry: false,
      bar: true,
      fill: null,
    };
  }
  if (earlier.kind === "held") {
    return {
      state: "held",
      lead: "",
      figure: figureOf(above),
      trail: "· pull to load",
      retry: false,
      bar: true,
      fill: null,
    };
  }
  return {
    state: "rest",
    lead: "",
    figure: figureOf(above),
    trail: "· pull to load",
    retry: false,
    bar: true,
    fill: "0",
  };
}
