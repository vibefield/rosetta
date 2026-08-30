// A unified diff, parsed to hunks. Pure: no git, no IO, no clock.

export { parseDiff } from "./parse";
export type { DiffAbsence, DiffHunk, DiffLine, DiffLineKind, ParsedDiff } from "./types";
