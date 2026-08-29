// Source, with a gutter and highlighting.
//
// Virtualized, because a viewer that will open any file in a worktree will be
// handed a 60,000-line lockfile eventually, and 60,000 rows of spans is a
// frozen window. The row height is declared HERE and applied as an inline
// style rather than left to the stylesheet: the virtualizer has to know the
// pitch it is scrolling, and two sources for one number is how a virtual list
// ends up drifting away from its own scrollbar.

import { useMemo, useRef } from "react";
import type { CSSProperties } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { highlight, PLAIN_TEXT } from "../syntax";
import type { TokenLine } from "../syntax";
import type { ViewBodyProps } from "../types";

/** One line's box, in px. Matches `--ah-code-line` in room.css, and wins. */
const LINE_HEIGHT = 19;
/** Rows drawn beyond the viewport, so a flick does not show blank bands. */
const OVERSCAN = 30;
/** A tab is drawn at this width, and counted at it for the scroll extent. */
const TAB_COLUMNS = 2;

/** How wide the widest line is, in monospace columns. Measured up front so
 * the horizontal extent is the document's, not "the widest row currently on
 * screen" — which changes as you scroll, and takes the scrollbar with it. */
function widestLine(lines: readonly TokenLine[]): number {
  let widest = 0;
  for (const line of lines) {
    let columns = 0;
    for (const token of line) {
      for (const char of token.text) columns += char === "\t" ? TAB_COLUMNS : 1;
    }
    if (columns > widest) widest = columns;
  }
  return widest;
}

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

  const columns = useMemo(() => widestLine(lines), [lines]);
  // Handed to CSS as a variable rather than set as a width here: the room
  // resets everything to `border-box`, so a width in `ch` with padding inside
  // it leaves the digits no room at all and the code lands on top of them.
  // The stylesheet adds the padding to this; nobody has to remember both.
  const digits = `${String(lines.length).length}ch`;

  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => scrollerRef.current,
    estimateSize: () => LINE_HEIGHT,
    overscan: OVERSCAN,
  });

  return (
    <div
      className="ah-fileview__scroll ah-code"
      ref={scrollerRef}
      style={{ "--ah-code-digits": digits } as CSSProperties}
    >
      <div
        className="ah-code__sizer"
        style={{
          height: virtualizer.getTotalSize(),
          minWidth: `calc(${columns}ch + var(--ah-code-gutter) + var(--ah-code-trail))`,
        }}
      >
        {virtualizer.getVirtualItems().map((row) => (
          <div
            className="ah-code__line"
            key={row.key}
            style={{ height: LINE_HEIGHT, transform: `translateY(${row.start}px)` }}
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
  );
}
