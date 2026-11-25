// What a worktree's folder holds on disk, for the room's worktree ledger.
//
// There is no instant answer for an ordinary folder: the figure is a walk.
// The walk's cost follows the number of files, not their bytes (measured
// 2026-11-24 on an APFS SSD, file cache warm: 36,000 entries and 1.05 GB in
// 0.2 s; a 3.7 GB build folder of 5,800 entries in 0.02 s), so it is cheap
// enough to do in the background and too slow to do while a reader waits.
// Hence the shape: `read` answers at once from what was measured and queues a
// measurement when it has none or an old one; one walk runs at a time, at low
// priority, and the room's next poll of its git facts carries the figure.
//
// `du` does the walking: it is on every Mac and Linux, counts a hard-linked
// file once, and with `-x` never leaves the filesystem it started on. It
// counts blocks allocated, so a sparse file counts what it holds. Two things
// the figure is not: what removing the folder would free (files cloned from a
// package store share their blocks), and anything behind a link.
//
// The folder's path comes from `git worktree list`, and a repository an agent
// wrote can name any folder there. So a path is only walked when it is
// absolute and has a `.git` of its own, a walk is cut at its timeout, and a
// cut or failed walk is remembered as "no figure" for as long as a figure
// would be, never retried in a loop.

import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";

/** How long a figure stands before the next read re-measures it. */
export const TREE_SIZE_TTL_MS = 10 * 60_000;
/** One walk's limit. A checkout of 300,000 files reads in a few seconds. */
export const TREE_SIZE_TIMEOUT_MS = 60_000;
/** Paths remembered; the oldest go first. */
const KEPT = 256;

/** `du -skx -- <folder>`: kilobytes allocated, in bytes; null when it could
 * not be read or ran out of time. `du` exits non-zero when one folder below
 * refused it and still prints the sum of the rest, which is kept. */
export function measureWithDu(folder, { timeoutMs = TREE_SIZE_TIMEOUT_MS, onChild } = {}) {
  return new Promise((resolve) => {
    const child = execFile(
      "du",
      ["-skx", "--", folder],
      {
        timeout: timeoutMs,
        maxBuffer: 64 * 1024,
        windowsHide: true,
        env: { PATH: process.env.PATH ?? "/usr/bin:/bin", LANG: "C", LC_ALL: "C" },
      },
      (error, stdout) => {
        if (error?.killed === true) return resolve(null);
        const kilobytes = /^(\d+)\s/.exec(String(stdout))?.[1];
        resolve(kilobytes === undefined ? null : Number(kilobytes) * 1024);
      },
    );
    try {
      if (child.pid !== undefined) os.setPriority(child.pid, 10);
    } catch {
      // it runs at the priority it has
    }
    onChild?.(child);
  });
}

/**
 * @param {{ ttlMs?: number, now?: () => number, platform?: string,
 *   measure?: (folder: string, options: { onChild: (child: import("node:child_process").ChildProcess) => void }) => Promise<number | null>,
 *   isWorktree?: (folder: string) => boolean }} [options]
 */
export function createTreeSizes({
  ttlMs = TREE_SIZE_TTL_MS,
  now = Date.now,
  platform = process.platform,
  measure = measureWithDu,
  isWorktree = (folder) => path.isAbsolute(folder) && existsSync(path.join(folder, ".git")),
} = {}) {
  /** @type {Map<string, { bytes: number | undefined, at: number }>} */
  const known = new Map();
  /** @type {string[]} */
  const queue = [];
  let running = null;
  let child = null;
  let disposed = false;

  const next = () => {
    if (running !== null || disposed) return;
    const folder = queue.shift();
    if (folder === undefined) return;
    running = folder;
    const done = (bytes) => {
      child = null;
      running = null;
      if (disposed) return;
      known.delete(folder);
      known.set(folder, { bytes: bytes ?? undefined, at: now() });
      while (known.size > KEPT) known.delete(known.keys().next().value);
      next();
    };
    Promise.resolve()
      .then(() => (isWorktree(folder) ? measure(folder, { onChild: (started) => { child = started; } }) : null))
      .then(done, () => done(null));
  };

  return {
    /** The figure for one folder: a number of bytes; null while a first
     * measurement is queued or running; undefined when this host does not
     * measure (Windows has no `du`) or the folder could not be read. An old
     * figure is answered while its replacement is measured. */
    read(folder) {
      if (platform === "win32" || disposed) return undefined;
      const entry = known.get(folder);
      if (entry === undefined || now() - entry.at >= ttlMs) {
        if (running !== folder && !queue.includes(folder)) queue.push(folder);
        next();
      }
      return entry === undefined ? null : entry.bytes;
    },
    /** Stop: the walk in flight is ended and nothing further starts. */
    dispose() {
      disposed = true;
      queue.length = 0;
      child?.kill();
    },
  };
}
