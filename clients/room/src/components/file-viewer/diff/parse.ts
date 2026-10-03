// Parsing one file's unified diff, as `git diff` writes it.
//
// One file per call. git will happily emit many in one run; the column asks
// per file, because the viewer shows one file and a parser that returns a
// list would only be unpacked at every call site.

import type { DiffAbsence, DiffHunk, DiffLine, ParsedDiff } from "./types";

/** `@@ -12,7 +12,9 @@ fn walk()` — counts are optional and default to 1. */
const HUNK = /^@@+ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@+(.*)$/;
/** Codex `apply_patch` also persists location-free `@@` hunks. */
const UNPOSITIONED_HUNK = /^@@(?:\s+(.*))?$/;

const EMPTY: ParsedDiff = {
  oldPath: null,
  newPath: null,
  hunks: [],
  absence: "unchanged",
  renamedFrom: null,
  added: 0,
  deleted: 0,
};

/** `a/src/main.rs` → `src/main.rs`; `/dev/null` → null. */
function pathOf(raw: string): string | null {
  const trimmed = raw.trim();
  if (trimmed === "/dev/null" || trimmed === "") return null;
  // git quotes a path containing anything unusual, and escapes it C-style.
  const unquoted =
    trimmed.startsWith('"') && trimmed.endsWith('"')
      ? trimmed.slice(1, -1).replace(/\\(.)/g, "$1")
      : trimmed;
  return unquoted.replace(/^[ab]\//, "");
}

/** The header's path, with the timestamp `--- a/x.ts\t2026-08-30` can carry. */
function headerPath(line: string, marker: string): string | null {
  return pathOf(line.slice(marker.length).split("\t")[0]!);
}

export function parseDiff(text: string): ParsedDiff {
  if (text.trim() === "") return EMPTY;

  const lines = text.split("\n");
  const hunks: DiffHunk[] = [];
  let oldPath: string | null = null;
  let newPath: string | null = null;
  let renamedFrom: string | null = null;
  let absence: DiffAbsence | null = null;
  let added = 0;
  let deleted = 0;

  // `---` and `+++` are authoritative — `/dev/null` on one of them is how an
  // add or a delete is spelled. `diff --git a/x b/y` is only a FALLBACK, for
  // a pure rename or mode change where those two lines do not appear; it must
  // never overwrite a `/dev/null` with a path, which is what an
  // already-assigned `??=` quietly did.
  let sawOldMarker = false;
  let sawNewMarker = false;
  let statedOld: string | null = null;
  let statedNew: string | null = null;

  let current: DiffLine[] | null = null;
  let oldNumber: number | null = 0;
  let newNumber: number | null = 0;

  const close = (header: string, oldStart: number | null, newStart: number | null): void => {
    if (current !== null && current.length > 0) {
      hunks.push({ header, oldStart, newStart, lines: current });
    }
    current = null;
  };
  let header = "";
  let oldStart: number | null = 0;
  let newStart: number | null = 0;

  for (const line of lines) {
    const hunk = HUNK.exec(line);
    if (hunk !== null) {
      close(header, oldStart, newStart);
      header = hunk[5]!.trim();
      oldStart = Number.parseInt(hunk[1]!, 10);
      newStart = Number.parseInt(hunk[3]!, 10);
      oldNumber = oldStart;
      newNumber = newStart;
      current = [];
      continue;
    }
    const unpositioned = UNPOSITIONED_HUNK.exec(line);
    if (unpositioned !== null) {
      close(header, oldStart, newStart);
      header = (unpositioned[1] ?? "").trim();
      oldStart = null;
      newStart = null;
      oldNumber = null;
      newNumber = null;
      current = [];
      continue;
    }

    if (current === null) {
      // Still in the file header: the preamble git writes before the hunks.
      if (line.startsWith("--- ") && !sawOldMarker) {
        sawOldMarker = true;
        oldPath = headerPath(line, "--- ");
      } else if (line.startsWith("+++ ") && !sawNewMarker) {
        sawNewMarker = true;
        newPath = headerPath(line, "+++ ");
      } else if (line.startsWith("rename from ")) renamedFrom = pathOf(line.slice(12));
      else if (line.startsWith("Binary files") || line.startsWith("GIT binary patch")) {
        absence = "binary";
      } else if (line.startsWith("diff --git ")) {
        // `diff --git a/x b/y`, which is the only place a path appears when
        // the change is a pure rename or a mode change — there are no `---`
        // and `+++` lines in that case.
        const both = line.slice(11);
        const half = Math.floor(both.length / 2);
        const split = both.lastIndexOf(" b/", half + 2);
        if (split !== -1) {
          statedOld = pathOf(both.slice(0, split));
          statedNew = pathOf(both.slice(split + 1));
        }
      } else if (line.startsWith("old mode ") || line.startsWith("new mode ")) {
        absence ??= "mode-only";
      }
      continue;
    }

    // Inside a hunk. An empty line is a context line whose leading space git
    // dropped — real diffs in the wild carry them, and treating one as the
    // end of the hunk truncates the file at its first blank line.
    const marker = line === "" ? " " : line[0];
    const body = line === "" ? "" : line.slice(1);

    if (marker === "+") {
      current.push({ kind: "add", text: body, oldNumber: null, newNumber });
      if (newNumber !== null) newNumber += 1;
      added += 1;
    } else if (marker === "-") {
      current.push({ kind: "del", text: body, oldNumber, newNumber: null });
      if (oldNumber !== null) oldNumber += 1;
      deleted += 1;
    } else if (marker === " ") {
      current.push({ kind: "context", text: body, oldNumber, newNumber });
      if (oldNumber !== null) oldNumber += 1;
      if (newNumber !== null) newNumber += 1;
    } else if (marker === "\\") {
      current.push({ kind: "note", text: body.trim(), oldNumber: null, newNumber: null });
    } else {
      // Anything else ends the hunk: the next file's header, or trailing
      // output the caller did not strip.
      close(header, oldStart, newStart);
    }
  }
  close(header, oldStart, newStart);

  return {
    oldPath: sawOldMarker ? oldPath : statedOld,
    newPath: sawNewMarker ? newPath : statedNew,
    hunks,
    absence: hunks.length > 0 ? null : (absence ?? "unchanged"),
    renamedFrom,
    added,
    deleted,
  };
}
