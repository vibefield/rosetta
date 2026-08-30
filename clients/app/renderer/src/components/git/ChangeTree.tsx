// One group's worth of changed files, as a subtree.
//
// The row is column ③'s row, with git's letter in front of the name: same
// glyph set, same 26px height, same indent, same order — because these are
// two views of ONE repository, and two lists that drew it differently would
// be two repositories as far as the eye is concerned. The glyph comes from
// mille's own `FileIcon`, which needs nothing but a name and a kind, so the
// two columns cannot drift apart as the theme changes.
//
// The letter is git's own, in the §2.5 hue its meaning earns: added is a
// positive delta, deleted a negative one, and everything else is a state the
// ramp can carry without a colour. A file this session touched wears a mark
// before its glyph — the one fact a general git client cannot show, and the
// reason column ⑤ is in a room about agent sessions at all.

import { FileIcon } from "@vibecook/mille-ui/icons";
import { vibefieldIconTheme } from "@vibecook/mille-ui/icons/vibefield";
import type { StatusLetter } from "./types";
import type { TreeRow } from "./path-tree";

export interface ChangeTreeProps {
  readonly rows: readonly TreeRow[];
  /** The letter each path earned, by path. */
  readonly letters: ReadonlyMap<string, StatusLetter>;
  /** Paths the open session touched. Empty until the typed tool projection
   * ships, which draws no marks rather than guessing at them. */
  readonly sessionPaths: ReadonlySet<string>;
  readonly selectedPath: string | null;
  readonly onSelect: (row: TreeRow) => void;
  /** The act offered on each row — staging or unstaging, depending on which
   * group this is. `null` while no client can serve it. */
  readonly action: { readonly label: string; readonly run: (row: TreeRow) => void } | null;
  readonly onDiscard: ((row: TreeRow) => void) | null;
}

/** What each of git's letters means, in words, for a tooltip. */
const MEANING: Readonly<Record<StatusLetter, string>> = {
  M: "modified",
  A: "added",
  D: "deleted",
  R: "renamed",
  C: "copied",
  T: "type changed",
  "?": "untracked",
  U: "conflicted",
};

export function ChangeTree({
  rows,
  letters,
  sessionPaths,
  selectedPath,
  onSelect,
  action,
  onDiscard,
}: ChangeTreeProps) {
  return (
    <div className="ah-changetree" role="list">
      {rows.map((row) => {
        const letter = letters.get(row.path) ?? null;
        const touched = sessionPaths.has(row.path);
        return (
          <div
            className="ah-changerow"
            key={`${row.kind}:${row.path}`}
            role="listitem"
            data-kind={row.kind}
            data-selected={row.path === selectedPath ? "" : undefined}
            style={{ paddingLeft: `${8 + row.depth * 14}px` }}
          >
            <button
              type="button"
              className="ah-changerow__name"
              title={row.path}
              onClick={() => onSelect(row)}
            >
              <i
                className="ah-changerow__mark"
                data-touched={touched ? "" : undefined}
                title={touched ? "this session touched this file" : undefined}
              />
              <FileIcon
                className="ah-changerow__icon"
                entry={{ name: row.label, kind: row.kind === "directory" ? 1 : 0 }}
                theme={vibefieldIconTheme}
                expanded={row.kind === "directory"}
              />
              {/* Before the name, where column ③ draws it. */}
              <code
                className="ah-changerow__letter"
                data-letter={letter ?? undefined}
                title={letter === null ? undefined : MEANING[letter]}
              >
                {letter ?? ""}
              </code>
              <span>{row.label}</span>
            </button>
            <span className="ah-changerow__acts">
              {onDiscard !== null && row.kind === "file" && (
                <button
                  type="button"
                  title="Discard changes to this file"
                  onClick={() => onDiscard(row)}
                >
                  ↩
                </button>
              )}
              {action !== null && (
                <button type="button" title={action.label} onClick={() => action.run(row)}>
                  {action.label === "Stage" ? "+" : "−"}
                </button>
              )}
            </span>
          </div>
        );
      })}
    </div>
  );
}
