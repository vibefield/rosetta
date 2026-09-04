// Source, with a gutter and highlighting.
//
// Virtualized, because a viewer that will open any file in a worktree will be
// handed a 60,000-line lockfile eventually, and 60,000 rows of spans is a
// frozen window. Nineteen pixels is the estimate and minimum row pitch; the
// virtualizer measures wrapped rows from their DOM boxes and corrects every
// following offset when the available width changes.

import { useCallback, useMemo, useRef } from "react";
import type { CSSProperties } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { CodeMinimap } from "../minimap/CodeMinimap";
import { highlight, PLAIN_TEXT } from "../syntax";
import type { ViewBodyProps } from "../types";

/** One visual row's minimum and unmeasured estimate, in CSS px. */
const LINE_HEIGHT = 19;
/** Rows drawn beyond the viewport, so a flick does not show blank bands. */
const OVERSCAN = 30;
/** A tab uses the same stop width in the source listing and minimap. */
const TAB_COLUMNS = 2;

export function CodeView({ file }: ViewBodyProps) {
  const scrollerRef = useRef<HTMLDivElement>(null);

  const lines = useMemo(() => {
    const source = file.text ?? "";
    const highlighted = highlight(source, file.kind.language ?? PLAIN_TEXT);
    // A file that ends in a newline is not a file with a blank last line;
    // every editor counts it the same way.
    return highlighted.length > 1 && highlighted[highlighted.length - 1].length === 0
      ? highlighted.slice(0, -1)
      : highlighted;
  }, [file]);

  // Handed to CSS as a variable so the wrapped source measure begins at one
  // stable gutter edge regardless of the number of lines in the file.
  const digits = `${String(lines.length).length}ch`;

  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => scrollerRef.current,
    estimateSize: () => LINE_HEIGHT,
    overscan: OVERSCAN,
  });
  const scrollToLine = useCallback(
    (lineIndex: number): void => {
      virtualizer.scrollToIndex(lineIndex, { align: "center" });
    },
    [virtualizer],
  );

  return (
    <div className="ah-code-shell">
      <div
        className="ah-fileview__scroll ah-code"
        ref={scrollerRef}
        style={{ "--ah-code-digits": digits } as CSSProperties}
      >
        <div
          className="ah-code__sizer"
          style={{ height: virtualizer.getTotalSize() }}
        >
          {virtualizer.getVirtualItems().map((row) => (
            <div
              className="ah-code__line"
              data-index={row.index}
              key={row.key}
              ref={virtualizer.measureElement}
              style={{ transform: `translateY(${row.start}px)` }}
            >
              <span className="ah-code__num">{row.index + 1}</span>
              <code>
                {lines[row.index].map((token, at) =>
                  token.kind === "plain" ? (
                    token.text
                  ) : (
                    // eslint-disable-next-line react/no-array-index-key -- the
                    // token list is derived from immutable text; index IS its id.
                    <span data-t={token.kind} key={at}>
                      {token.text}
                    </span>
                  ),
                )}
              </code>
            </div>
          ))}
        </div>
      </div>
      <CodeMinimap
        lines={lines}
        lineHeight={LINE_HEIGHT}
        tabColumns={TAB_COLUMNS}
        scrollerRef={scrollerRef}
        scrollToLine={scrollToLine}
      />
    </div>
  );
}
