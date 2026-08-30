// Which files the open session touched.
//
// The one fact a general git client cannot show, and the reason a git column
// belongs in a room about agent sessions. It is derived from the timeline the
// room is already reading — every `file_change` tool action names a path — so
// it costs no new request and no new table.
//
// Until the typed tool projection ships, the daemon answers no `file_change`
// actions at all and this returns an empty set. That draws NO marks, which is
// the honest state: "we cannot say" and "this session touched nothing" look
// the same here, and neither of them is a lie about a file.

import type { TimelineEntry } from "@rosetta/protocol";

/** `path` on a `ResourceRef`, if that is what this value is. */
function refPath(value: unknown): string | null {
  if (value === null || typeof value !== "object") return null;
  const path = (value as { path?: unknown }).path;
  return typeof path === "string" && path !== "" ? path : null;
}

/**
 * Workspace-relative paths this session's file-change actions named.
 *
 * Absolute paths are made relative to `root` so they match what git prints;
 * a path outside the root is dropped rather than shown against the wrong
 * repository.
 */
export function sessionFilePaths(
  entries: readonly TimelineEntry[],
  root: string | null,
): ReadonlySet<string> {
  const out = new Set<string>();
  if (root === null) return out;
  const prefix = root.endsWith("/") ? root : `${root}/`;

  for (const entry of entries) {
    const body = (entry as { body?: { type?: string; tool?: unknown } }).body;
    if (body?.type !== "tool_use") continue;
    const action = (body.tool as { action?: { type?: string } } | undefined)?.action;
    if (action?.type !== "file_change") continue;

    // Both the changed path and a move's destination count as touched.
    const change = action as { path?: unknown; destination?: unknown };
    for (const candidate of [refPath(change.path), refPath(change.destination)]) {
      if (candidate === null) continue;
      if (candidate.startsWith(prefix)) out.add(candidate.slice(prefix.length));
      else if (!candidate.startsWith("/")) out.add(candidate);
    }
  }
  return out;
}
