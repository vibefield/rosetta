// Squarified treemap layout (Bruls, Huizing, van Wijk 2000): take items
// largest first, keep adding to the current row while doing so improves its
// worst aspect ratio, then lay the row along the short side and recurse into
// what is left. Computed against the box's real proportions rather than a
// square, or the tiles come out squarified for a box that does not exist.

export interface TileBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

/* the box's real inner size: 260 column − 24 body padding − 18 card padding */
export const TREE_W = 218;
export const TREE_H = 110;

/* The same floor the ledger bar has, in two dimensions: a project at 0.2% of
   the spend would be four pixels wide, which is not a control anyone can hit.
   Tiles below this share of the box are drawn at it and the rest are
   renormalised around them — and the footer says how many were floored,
   because a floored area is no longer to scale. */
export const TREE_FLOOR = 0.009;

function sum(values: number[]): number {
  return values.reduce((t, v) => t + v, 0);
}

/** Lay `areas` (absolute, summing to W×H, sorted descending) into the box. */
export function squarify(areas: number[], W: number, H: number): TileBox[] {
  const out: TileBox[] = [];
  let x = 0;
  let y = 0;
  let w = W;
  let h = H;
  let i = 0;
  let row: number[] = [];
  let rowAcross = false;
  let rowLen = 0;

  const worst = (r: number[], len: number): number => {
    if (r.length === 0) return Infinity;
    const t = sum(r);
    const mx = Math.max(...r);
    const mn = Math.min(...r);
    return Math.max((len * len * mx) / (t * t), (t * t) / (len * len * mn));
  };

  const flush = () => {
    const t = sum(row);
    const thick = t / rowLen;
    let pos = 0;
    for (const a of row) {
      const side = a / thick;
      out.push(
        rowAcross
          ? { x: x + pos, y, w: side, h: thick }
          : { x, y: y + pos, w: thick, h: side },
      );
      pos += side;
    }
    if (rowAcross) {
      y += thick;
      h -= thick;
    } else {
      x += thick;
      w -= thick;
    }
    row = [];
  };

  while (i < areas.length) {
    /* rows run along the SHORT side — that is what keeps tiles near-square */
    if (row.length === 0) {
      rowAcross = h > w;
      rowLen = rowAcross ? w : h;
    }
    if (worst([...row, areas[i]], rowLen) <= worst(row, rowLen)) {
      row.push(areas[i]);
      i += 1;
    } else {
      flush();
    }
  }
  if (row.length > 0) flush();
  return out;
}

export interface TreemapTile<T> {
  item: T;
  box: TileBox;
  /** True share of the total, before any flooring. */
  share: number;
  floored: boolean;
  /** Whether the tile has room for its name and figure. */
  labelled: boolean;
}

/** Floor, renormalise, and lay out; items must arrive sorted descending. */
export function treemapTiles<T>(
  items: T[],
  valueOf: (item: T) => number,
): { tiles: TreemapTile<T>[]; floored: number; unlabelled: number } {
  const total = sum(items.map(valueOf)) || 1;
  let floored = 0;
  const raw = items.map((item) => {
    const share = valueOf(item) / total;
    if (share >= TREE_FLOOR) return share;
    floored += 1;
    return TREE_FLOOR;
  });
  const scale = (TREE_W * TREE_H) / (sum(raw) || 1);
  const boxes = squarify(raw.map((v) => v * scale), TREE_W, TREE_H);
  let unlabelled = 0;
  const tiles = boxes.map((box, i) => {
    const share = valueOf(items[i]) / total;
    const labelled = box.w >= 46 && box.h >= 27;
    if (!labelled) unlabelled += 1;
    return { item: items[i], box, share, floored: share < TREE_FLOOR, labelled };
  });
  return { tiles, floored, unlabelled };
}
