// A unified diff, parsed.
//
// The viewer displays diffs; it does not compute them. git already knows
// which lines changed — including the rename and binary cases that a naive
// line differ gets wrong — so the room reads git's answer rather than
// deriving a second one that can disagree with it.

export type DiffLineKind =
  /** Unchanged, shown on both sides. */
  | "context"
  | "add"
  | "del"
  /** `\ No newline at end of file` — git's own note about the line above. */
  | "note";

export interface DiffLine {
  readonly kind: DiffLineKind;
  readonly text: string;
  /** Line number on the left, or null for an added line. */
  readonly oldNumber: number | null;
  /** Line number on the right, or null for a removed line. */
  readonly newNumber: number | null;
}

export interface DiffHunk {
  /** The `@@ … @@` line, whose trailing text git fills with the enclosing
   * function or section — worth showing, and not ours to invent. */
  readonly header: string;
  /** Null when the source persisted a hunk but omitted its coordinates, as
   * Codex's `apply_patch` dialect may do with a bare `@@`. */
  readonly oldStart: number | null;
  readonly newStart: number | null;
  readonly lines: readonly DiffLine[];
}

/** Why a diff has no hunks, when it has none. */
export type DiffAbsence =
  /** The two sides are identical. */
  | "unchanged"
  /** git declined to diff it: `Binary files … differ`. */
  | "binary"
  /** The file's mode changed and its content did not. */
  | "mode-only";

export interface ParsedDiff {
  /** Path on the left, or null when the file is being added. */
  readonly oldPath: string | null;
  /** Path on the right, or null when the file is being deleted. */
  readonly newPath: string | null;
  readonly hunks: readonly DiffHunk[];
  /** Set when `hunks` is empty and that is a fact rather than an error. */
  readonly absence: DiffAbsence | null;
  /** `old → new` when git detected a rename. */
  readonly renamedFrom: string | null;
  readonly added: number;
  readonly deleted: number;
}
