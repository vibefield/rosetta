// A file's change, as git described it.
//
// Unified rather than side-by-side: this dialog is 1080px at most and a
// split view halves the measure twice over — once for the gutter, once for
// the second pane — and the room's own transcript already argues that a
// narrow column of code is the thing that suffers first.
//
// The line hues are §2.5's, and they are not an exception: DESIGN.md defines
// `--vf-green` as "positive delta" and `--vf-red` as "negative delta"
// outright. What stays neutral is the diffstat FIGURE — §8's rule that hue
// says which project or which state, and `+214 −38` is neither.

import { useMemo, useRef } from "react";
import type { CSSProperties } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { parseDiff } from "../diff";
import type { DiffLine } from "../diff";
import { highlight, languageFor, PLAIN_TEXT } from "../syntax";
import type { TokenLine } from "../syntax";
import type { ViewBodyProps } from "../types";

/** Matches `--ah-code-line`; the virtualizer scrolls by it. */
const LINE_HEIGHT = 19;
const OVERSCAN = 24;

/** One drawn row: a hunk header, or one of its lines. */
type Row =
  | { readonly kind: "hunk"; readonly header: string; readonly range: string }
  | { readonly kind: "line"; readonly line: DiffLine; readonly tokens: TokenLine };

const MARK: Record<DiffLine["kind"], string> = {
  add: "+",
  del: "−",
  context: " ",
  note: "",
};

export function DiffView({ file }: ViewBodyProps) {
  const scrollerRef = useRef<HTMLDivElement>(null);

  const { rows, parsed } = useMemo(() => {
    const diff = parseDiff(file.unified ?? "");
    // Every line is highlighted in the file's own language — a diff of Rust
    // should read as Rust. The tokenizer runs per line here rather than over
    // the file, so a block comment that opens outside the hunk is not known
    // to be open; that is the honest cost of only having the hunks.
    const language = languageFor(file.handle.name) ?? PLAIN_TEXT;
    const out: Row[] = [];
    for (const hunk of diff.hunks) {
      out.push({
        kind: "hunk",
        header: hunk.header,
        range: `@@ −${hunk.oldStart} +${hunk.newStart} @@`,
      });
      for (const line of hunk.lines) {
        out.push({
          kind: "line",
          line,
          tokens: line.kind === "note" ? [] : (highlight(line.text, language)[0] ?? []),
        });
      }
    }
    return { rows: out, parsed: diff };
  }, [file]);

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollerRef.current,
    estimateSize: () => LINE_HEIGHT,
    overscan: OVERSCAN,
  });

  if (rows.length === 0) {
    const why =
      parsed.absence === "binary"
        ? "git will not diff a binary file, and neither will this."
        : parsed.absence === "mode-only"
          ? "Only the file's mode changed. Its content is identical."
          : parsed.renamedFrom !== null
            ? `Renamed from ${parsed.renamedFrom}, with no change to its content.`
            : "The two sides are identical.";
    return (
      <div className="ah-fileview__state">
        <b>No lines changed</b>
        <span>{why}</span>
      </div>
    );
  }

  const gutter = `${String(
    Math.max(
      ...rows.map((row) =>
        row.kind === "line" ? Math.max(row.line.oldNumber ?? 0, row.line.newNumber ?? 0) : 0,
      ),
    ),
  ).length}ch`;

  return (
    <div
      className="ah-fileview__scroll ah-code ah-diff"
      ref={scrollerRef}
      style={{ "--ah-code-digits": gutter } as CSSProperties}
    >
      {/* The diffstat is NEUTRAL — §8's rule that hue says which project or
          which state, and a direction of change is neither. The LINES wear
          §2.5's deltas; the figure about them does not. */}
      <p className="ah-diff__stat">
        <span className="tnum">
          +{parsed.added} −{parsed.deleted}
        </span>
        {parsed.renamedFrom !== null && (
          <span className="ah-diff__renamed" title={parsed.renamedFrom}>
            renamed from {parsed.renamedFrom}
          </span>
        )}
      </p>
      <div className="ah-code__sizer" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((item) => {
          const row = rows[item.index];
          return (
            <div
              className="ah-code__line"
              key={item.key}
              data-diff={row.kind === "line" ? row.line.kind : "hunk"}
              style={{ height: LINE_HEIGHT, transform: `translateY(${item.start}px)` }}
            >
              {row.kind === "hunk" ? (
                <span className="ah-diff__hunk">
                  <span className="ah-diff__range tnum">{row.range}</span>
                  {row.header !== "" && <span className="ah-diff__context">{row.header}</span>}
                </span>
              ) : (
                <>
                  <span className="ah-code__num">{row.line.oldNumber ?? ""}</span>
                  <span className="ah-code__num">{row.line.newNumber ?? ""}</span>
                  <span className="ah-diff__mark">{MARK[row.line.kind]}</span>
                  <code>
                    {row.line.kind === "note" ? (
                      <em>{row.line.text}</em>
                    ) : (
                      row.tokens.map((token, at) =>
                        token.kind === "plain" ? (
                          token.text
                        ) : (
                          <span data-t={token.kind} key={at}>
                            {token.text}
                          </span>
                        ),
                      )
                    )}
                  </code>
                </>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
