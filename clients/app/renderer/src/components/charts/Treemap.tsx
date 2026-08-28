import { useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { createTreemapLayout } from "./treemap-layout";
import type { TreemapData, TreemapItemData } from "./types";

const DEFAULT_LAYOUT_WIDTH = 218;
const DEFAULT_HEIGHT = 110;

type TreemapItemStyle = CSSProperties & { "--treemap-item-color": string };

export interface TreemapItemContext {
  readonly share: number;
  readonly isMinimumSized: boolean;
  readonly isLabelVisible: boolean;
}

export interface TreemapItemNoun {
  readonly singular: string;
  readonly plural: string;
}

export interface TreemapProps {
  readonly data: TreemapData;
  readonly ariaLabel: string;
  readonly title?: string;
  readonly selectedItemId?: string | null;
  readonly onItemSelect?: (item: TreemapItemData) => void;
  readonly note?: string;
  readonly showFooter?: boolean;
  readonly emptyMessage?: string;
  readonly itemNoun?: TreemapItemNoun;
  readonly height?: number;
  readonly minimumVisibleShare?: number;
  readonly formatValue?: (value: number) => string;
  readonly getItemAccessibleLabel?: (
    item: TreemapItemData,
    context: TreemapItemContext,
  ) => string;
}

function defaultValueFormatter(value: number): string {
  return value.toLocaleString();
}

function countLabel(
  count: number,
  { singular, plural }: TreemapItemNoun,
): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

function percentage(value: number, total: number): string {
  return `${((value / total) * 100).toFixed(3)}%`;
}

export function Treemap({
  data,
  title,
  ariaLabel,
  selectedItemId = null,
  onItemSelect,
  note,
  showFooter = true,
  emptyMessage = "No data in this period",
  itemNoun = { singular: "item", plural: "items" },
  height = DEFAULT_HEIGHT,
  minimumVisibleShare,
  formatValue = defaultValueFormatter,
  getItemAccessibleLabel,
}: TreemapProps) {
  const chartHeight = Number.isFinite(height) && height > 0 ? height : DEFAULT_HEIGHT;
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerWidth, setContainerWidth] = useState(DEFAULT_LAYOUT_WIDTH);

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const updateWidth = (nextWidth: number) => {
      if (!Number.isFinite(nextWidth) || nextWidth <= 0) return;
      setContainerWidth((currentWidth) =>
        Math.abs(currentWidth - nextWidth) < 0.5 ? currentWidth : nextWidth,
      );
    };

    updateWidth(container.getBoundingClientRect().width);
    if (typeof ResizeObserver === "undefined") return;

    const observer = new ResizeObserver(([entry]) => {
      if (entry) updateWidth(entry.contentRect.width);
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  const layout = useMemo(
    () =>
      createTreemapLayout(data.items, {
        width: containerWidth,
        height: chartHeight,
        minimumVisibleShare,
      }),
    [chartHeight, containerWidth, data, minimumVisibleShare],
  );
  const caveats: string[] = [];
  if (layout.hiddenLabelCount > 0) {
    caveats.push(`${layout.hiddenLabelCount} labels hidden`);
  }
  if (layout.minimumSizedItemCount > 0) {
    caveats.push(`${layout.minimumSizedItemCount} minimum-sized`);
  }
  const omittedItemCount = data.items.length - layout.tiles.length;
  if (omittedItemCount > 0) {
    caveats.push(`${omittedItemCount} without area`);
  }
  const layoutNote =
    caveats.length > 0
      ? caveats.join(" · ")
      : layout.tiles.length > 0
        ? "every tile true to scale"
        : "no positive values";

  return (
    <div className="ah-treemap">
      {(title || note) && (
        <div className="ah-chart-header">
          {title && <p className="ah-chart-header__title">{title}</p>}
          {note && <span className="ah-chart-header__note">{note}</span>}
        </div>
      )}

      <div
        ref={containerRef}
        className="ah-treemap__plot"
        style={{ height: chartHeight }}
        role="group"
        aria-label={ariaLabel}
      >
        {layout.tiles.length === 0 ? (
          <p className="ah-treemap__empty">{emptyMessage}</p>
        ) : (
          layout.tiles.map(
            ({ item, rectangle, share, isMinimumSized, canShowLabel }) => {
              const context: TreemapItemContext = {
                share,
                isMinimumSized,
                isLabelVisible: canShowLabel,
              };
              const accessibleLabel = getItemAccessibleLabel
                ? getItemAccessibleLabel(item, context)
                : `${item.label} · ${formatValue(item.value)} · ${(share * 100).toFixed(1)}% of total${
                    isMinimumSized ? " · shown at the minimum visible size" : ""
                  }`;
              const isSelected = item.id === selectedItemId;
              const className = `ah-treemap__item${isSelected ? " is-selected" : ""}`;
              const itemStyle = {
                "--treemap-item-color": item.color,
                left: percentage(rectangle.x, containerWidth),
                top: percentage(rectangle.y, chartHeight),
                width: percentage(rectangle.width, containerWidth),
                height: percentage(rectangle.height, chartHeight),
              } as TreemapItemStyle;
              const itemContent = (
                <span className="ah-treemap__item-content">
                  {canShowLabel && (
                    <>
                      <b>{item.label}</b>
                      <em>{formatValue(item.value)}</em>
                    </>
                  )}
                </span>
              );

              return onItemSelect ? (
                <button
                  key={item.id}
                  type="button"
                  className={className}
                  style={itemStyle}
                  aria-label={accessibleLabel}
                  aria-pressed={isSelected}
                  title={accessibleLabel}
                  onClick={() => onItemSelect(item)}
                >
                  {itemContent}
                </button>
              ) : (
                <div
                  key={item.id}
                  className={className}
                  style={itemStyle}
                  role="img"
                  aria-label={accessibleLabel}
                  title={accessibleLabel}
                >
                  {itemContent}
                </div>
              );
            },
          )
        )}
      </div>

      {showFooter && (
        <div className="ah-treemap__footer">
          <span>{countLabel(data.items.length, itemNoun)}</span>
          <span>{layoutNote}</span>
        </div>
      )}
    </div>
  );
}
