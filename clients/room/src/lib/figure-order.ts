// The order project figures hydrate in after publication, and how a failed
// one is asked again (ux:X6). Figures wait on one global gate — the daemon
// publishes aggregates only once the whole import has finished — so the
// minute after it is when they all land at once, four at a time. The reader
// is looking at one project and a screenful of cards: those go first, then
// the rest of the list as drawn, then any project the list does not draw.
// Nothing here changes what is asked, only when.

/** A project card as the list draws it. */
export interface DrawnCard {
  /** The project id (the unassigned bucket's id included). */
  readonly id: string;
  /** Any part of the card lies inside its scrolling list. */
  readonly onScreen: boolean;
}

/**
 * Every id in `ids`, once: the selected project, then the on-screen cards
 * top to bottom, then the other drawn cards top to bottom, then the
 * projects the list does not draw (folded sections, rows not mounted), by
 * id. Cards for projects not in `ids` are ignored.
 */
export function figureOrder(
  ids: readonly string[],
  selected: string | null,
  drawn: readonly DrawnCard[],
): string[] {
  const wanted = new Set(ids);
  const order: string[] = [];
  const take = (id: string) => {
    if (!wanted.delete(id)) return;
    order.push(id);
  };
  if (selected !== null) take(selected);
  for (const card of drawn) if (card.onScreen) take(card.id);
  for (const card of drawn) take(card.id);
  for (const id of [...wanted].sort()) take(id);
  return order;
}

/**
 * The project cards under `root`, top to bottom, each marked whether it
 * shows inside its scrolling list. One layout read per card; the hook asks
 * once per round, never per project.
 */
export function drawnProjectCards(root: ParentNode | null | undefined): DrawnCard[] {
  if (root === null || root === undefined) return [];
  const cards = Array.from(
    root.querySelectorAll<HTMLElement>(".ah-project-list-item[data-motion-key]"),
    (card) => {
      const box = card.getBoundingClientRect();
      const view = card.closest(".ah-scroll")?.getBoundingClientRect() ?? null;
      const onScreen = box.height > 0 && (view === null || (box.bottom > view.top && box.top < view.bottom));
      return { id: card.dataset.motionKey ?? "", top: box.top, onScreen };
    },
  ).filter((card) => card.id !== "");
  cards.sort((left, right) => left.top - right.top);
  return cards.map(({ id, onScreen }) => ({ id, onScreen }));
}

/** First retry after a failed figure, in ms. */
export const FIGURE_RETRY_MS = 8_000;
/** The longest wait between retries, in ms. */
export const FIGURE_RETRY_CAP_MS = 120_000;

/**
 * How long to wait before asking a failed project again: 8 s after the
 * first failure, doubling with each one after, at most two minutes — a
 * daemon mid-restart is back within seconds, a project that keeps failing
 * is not hammered, and it is still asked again while the panel says
 * "retrying". Only the failed projects are asked again.
 */
export function figureRetryDelay(failures: number): number {
  return Math.min(FIGURE_RETRY_CAP_MS, FIGURE_RETRY_MS * 2 ** Math.max(0, failures - 1));
}

/** The portfolio's line for figures that failed, e.g.
 * `figures for 1 project failed · retrying`. */
export function failedFiguresLine(count: number): string {
  return `figures for ${count.toLocaleString("en-US")} ${count === 1 ? "project" : "projects"} failed · retrying`;
}
