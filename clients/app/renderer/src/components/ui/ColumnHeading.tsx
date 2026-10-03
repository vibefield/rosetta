// The column head, once. Every column that names the thing it is showing —
// ① a portfolio, ② a project, ③ a worktree, ④ a session — is naming the same
// kind of object at a different depth, so room.css states outright that they
// are one component whose parts may never drift apart: tint marker, name,
// count. This is that component; the stylesheet is its other half.

import type { CSSProperties } from "react";

export interface ColumnHeadingProps {
  /** What this column is showing. Truncates with an ellipsis; the head keeps
   * its band whatever the name's length. */
  readonly name: string;
  /**
   * The §2.6 accent for the marker. `null` leaves the marker to inherit
   * whatever `--tint` the surrounding surface sets — which is not the same
   * as "no tint", and is how a head inside an already-tinted panel avoids
   * naming the same colour twice.
   */
  readonly tint?: string | null;
  /** A fact about the name, so it travels WITH it rather than sitting at the
   * far end of the band. Omitted when the column has no honest count. */
  readonly count?: number | null;
  /** What the count counts, for its tooltip. */
  readonly countTitle?: string | undefined;
  /** Ids — paths, branches, shas — set in mono. The head keeps its size and
   * weight; only the face changes. */
  readonly mono?: boolean;
  /**
   * The name's tooltip. Defaults to the name, which is right whenever the
   * name is the whole truth; pass the longer form when the head shows an
   * abbreviation, or `null` for a name that needs no second telling.
   */
  readonly title?: string | null | undefined;
}

export function ColumnHeading({
  name,
  tint = null,
  count = null,
  countTitle,
  mono = false,
  title,
}: ColumnHeadingProps) {
  // `undefined` means "not specified", which is different from an explicit
  // `null` asking for no tooltip at all.
  const nameTitle = title === undefined ? name : title;
  return (
    <div className="ah-colhead">
      <i style={tint !== null ? ({ "--tint": tint } as CSSProperties) : undefined} />
      <strong className={mono ? "ah-idname" : undefined} title={nameTitle ?? undefined}>
        {name}
      </strong>
      {count !== null && (
        <span className="ah-bubble" title={countTitle}>
          {count}
        </span>
      )}
    </div>
  );
}
