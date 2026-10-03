import type { TreemapItemData } from "./types";

export interface TreemapRectangle {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface TreemapTileLayout {
  item: TreemapItemData;
  rectangle: TreemapRectangle;
  /** Exact share of the total before applying the minimum visible area. */
  share: number;
  isMinimumSized: boolean;
  canShowLabel: boolean;
}

export interface TreemapLayout {
  tiles: readonly TreemapTileLayout[];
  hiddenLabelCount: number;
  minimumSizedItemCount: number;
}

export interface TreemapLayoutOptions {
  width: number;
  height: number;
  minimumVisibleShare?: number | undefined;
  minimumLabelWidth?: number;
  minimumLabelHeight?: number;
}

const DEFAULT_MINIMUM_VISIBLE_SHARE = 0.009;
const DEFAULT_MINIMUM_LABEL_WIDTH = 46;
// Two 11px lines plus the tile's top/bottom insets and a safe interline gap.
const DEFAULT_MINIMUM_LABEL_HEIGHT = 38;

function sum(values: readonly number[]): number {
  return values.reduce((total, value) => total + value, 0);
}

function worstAspectRatio(areas: readonly number[], rowLength: number): number {
  if (areas.length === 0 || rowLength <= 0) return Number.POSITIVE_INFINITY;

  const area = sum(areas);
  let largest = Number.NEGATIVE_INFINITY;
  let smallest = Number.POSITIVE_INFINITY;
  for (const itemArea of areas) {
    largest = Math.max(largest, itemArea);
    smallest = Math.min(smallest, itemArea);
  }
  if (area <= 0 || smallest <= 0) return Number.POSITIVE_INFINITY;

  return Math.max(
    (rowLength * rowLength * largest) / (area * area),
    (area * area) / (rowLength * rowLength * smallest),
  );
}

/** Squarified treemap layout by Bruls, Huizing, and van Wijk. */
function squarify(
  areas: readonly number[],
  containerWidth: number,
  containerHeight: number,
): TreemapRectangle[] {
  const rectangles: TreemapRectangle[] = [];
  let remainingX = 0;
  let remainingY = 0;
  let remainingWidth = containerWidth;
  let remainingHeight = containerHeight;
  let areaIndex = 0;
  let currentRow: number[] = [];
  let rowRunsHorizontally = false;
  let rowLength = 0;

  const placeCurrentRow = () => {
    const rowArea = sum(currentRow);
    const rowThickness = rowLength > 0 ? rowArea / rowLength : 0;
    let rowOffset = 0;

    for (const area of currentRow) {
      const itemLength = rowThickness > 0 ? area / rowThickness : 0;
      rectangles.push(
        rowRunsHorizontally
          ? {
              x: remainingX + rowOffset,
              y: remainingY,
              width: itemLength,
              height: rowThickness,
            }
          : {
              x: remainingX,
              y: remainingY + rowOffset,
              width: rowThickness,
              height: itemLength,
            },
      );
      rowOffset += itemLength;
    }

    if (rowRunsHorizontally) {
      remainingY += rowThickness;
      remainingHeight = Math.max(0, remainingHeight - rowThickness);
    } else {
      remainingX += rowThickness;
      remainingWidth = Math.max(0, remainingWidth - rowThickness);
    }
    currentRow = [];
  };

  while (areaIndex < areas.length) {
    if (currentRow.length === 0) {
      rowRunsHorizontally = remainingHeight > remainingWidth;
      rowLength = rowRunsHorizontally ? remainingWidth : remainingHeight;
    }

    const candidate = areas[areaIndex]!;
    if (
      currentRow.length === 0 ||
      worstAspectRatio([...currentRow, candidate], rowLength) <=
        worstAspectRatio(currentRow, rowLength)
    ) {
      currentRow.push(candidate);
      areaIndex += 1;
    } else {
      placeCurrentRow();
    }
  }

  if (currentRow.length > 0) placeCurrentRow();
  return rectangles;
}

function isPositiveFinite(value: number): boolean {
  return Number.isFinite(value) && value > 0;
}

/**
 * Sorts, normalises, and lays out treemap items. Zero and invalid values are
 * omitted because they have no meaningful area.
 */
export function createTreemapLayout(
  items: readonly TreemapItemData[],
  {
    width,
    height,
    minimumVisibleShare = DEFAULT_MINIMUM_VISIBLE_SHARE,
    minimumLabelWidth = DEFAULT_MINIMUM_LABEL_WIDTH,
    minimumLabelHeight = DEFAULT_MINIMUM_LABEL_HEIGHT,
  }: TreemapLayoutOptions,
): TreemapLayout {
  if (
    !Number.isFinite(width) ||
    !Number.isFinite(height) ||
    width <= 0 ||
    height <= 0
  ) {
    return { tiles: [], hiddenLabelCount: 0, minimumSizedItemCount: 0 };
  }

  const sortedItems = items
    .filter((item) => isPositiveFinite(item.value))
    .map((item, originalIndex) => ({ item, originalIndex }))
    .sort(
      (left, right) =>
        right.item.value - left.item.value || left.originalIndex - right.originalIndex,
    )
    .map(({ item }) => item);
  const total = sum(sortedItems.map((item) => item.value));
  if (total <= 0) {
    return { tiles: [], hiddenLabelCount: 0, minimumSizedItemCount: 0 };
  }

  const safeMinimumShare = Number.isFinite(minimumVisibleShare)
    ? Math.max(0, minimumVisibleShare)
    : DEFAULT_MINIMUM_VISIBLE_SHARE;
  const safeMinimumLabelWidth = Number.isFinite(minimumLabelWidth)
    ? Math.max(0, minimumLabelWidth)
    : DEFAULT_MINIMUM_LABEL_WIDTH;
  const safeMinimumLabelHeight = Number.isFinite(minimumLabelHeight)
    ? Math.max(0, minimumLabelHeight)
    : DEFAULT_MINIMUM_LABEL_HEIGHT;
  const exactShares = sortedItems.map((item) => item.value / total);
  const visualShares = exactShares.map((share) => Math.max(share, safeMinimumShare));
  const areaScale = (width * height) / sum(visualShares);
  const rectangles = squarify(
    visualShares.map((share) => share * areaScale),
    width,
    height,
  );

  let hiddenLabelCount = 0;
  let minimumSizedItemCount = 0;
  const tiles = rectangles.map((rectangle, index): TreemapTileLayout => {
    const share = exactShares[index]!;
    const isMinimumSized = share < safeMinimumShare;
    const canShowLabel =
      rectangle.width >= safeMinimumLabelWidth &&
      rectangle.height >= safeMinimumLabelHeight;
    if (isMinimumSized) minimumSizedItemCount += 1;
    if (!canShowLabel) hiddenLabelCount += 1;

    return {
      item: sortedItems[index]!,
      rectangle,
      share,
      isMinimumSized,
      canShowLabel,
    };
  });

  return { tiles, hiddenLabelCount, minimumSizedItemCount };
}
