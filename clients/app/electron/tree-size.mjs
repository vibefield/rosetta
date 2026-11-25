// What a worktree's folder holds on disk, for the room's worktree ledger.
//
// There is no instant answer for an ordinary folder: the figure is a walk.
// Its cost follows the number of files, not their bytes (measured 2026-11-24
// on an APFS SSD, file cache warm: 36,000 entries and 1.05 GB in 0.2 s; a
// 3.7 GB build folder of 5,800 entries in 0.02 s). One walk is cheap. Four
// hundred are not: the first cut of this queued every worktree git listed and
// measured each again ten minutes later, and a repository with 470 worktrees
// made that a walk without end that ran a laptop hot. So:
//
//  · Nothing is walked unless it is asked for by name (`ask`), and the room
//    asks only for the rows it is showing. A new ask REPLACES the queue: a row
//    scrolled away is no longer owed.
//  · A figure is kept, on disk (`file`), and stands for a day. Opening the
//    list again reads the file and walks nothing. `peek` answers a standing
//    figure however old, and queues nothing.
//  · A walk runs below the person's own work and rests: at the lowest
//    priority, on macOS clamped to utility quality of service
//    (`taskpolicy -c utility`); and after each walk the queue rests as long
//    as the walk took, so the walker never holds more than half of one core
//    however long the queue. (The first cut of this point used background
//    quality of service and twice the rest. Background throttles the disk:
//    a cold 0.87 GB checkout walked in 32 s there and in 0.9 s unclamped,
//    and the list showed one figure every half minute.)
//
// `du` does the walking: it is on every Mac and Linux, counts a hard-linked
// file once, and with `-x` never leaves the filesystem it started on. Two
// things the figure is not: what removing the folder would free (files cloned
// from a package store share their blocks), and anything behind a link.
//
// A path comes from `git worktree list`, and a repository an agent wrote can
// name any folder there. The caller asks only for paths git listed for the
// open root; here a path is walked only when it is absolute and has a `.git`
// of its own, a walk is cut at its timeout, and a cut or failed walk is
// remembered as "no figure" for a while, never retried in a loop.

import { execFile } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";

/** How long a figure stands before an ask walks the folder again. */
export const TREE_SIZE_TTL_MS = 24 * 60 * 60_000;
/** How long a failed or cut walk is remembered as "no figure". */
export const TREE_SIZE_FAILED_TTL_MS = 10 * 60_000;
/** One walk's limit. A checkout of 300,000 files reads in a few seconds. */
export const TREE_SIZE_TIMEOUT_MS = 60_000;
/** Folders one ask may name, and so the longest the queue gets. */
export const TREE_SIZE_ASK_MAX = 32;
/** After a walk the queue rests this many times the walk's own time… */
export const TREE_SIZE_REST_FACTOR = 1;
/** …and this long at least. */
export const TREE_SIZE_REST_MIN_MS = 100;
/** Paths remembered; the oldest go first. */
const KEPT = 4096;
/** How long new figures wait to be written together. */
const SAVE_DELAY_MS = 2_000;
/** macOS's door to a quality of service clamp; absent elsewhere. */
const TASKPOLICY = "/usr/sbin/taskpolicy";

/** `du -skx -- <folder>`: kilobytes allocated, in bytes; null when it could
 * not be read or ran out of time. `du` exits non-zero when one folder below
 * refused it and still prints the sum of the rest, which is kept. */
export function measureWithDu(folder, { timeoutMs = TREE_SIZE_TIMEOUT_MS, onChild, clamp } = {}) {
  const quiet = clamp ?? (process.platform === "darwin" && existsSync(TASKPOLICY));
  const du = ["-skx", "--", folder];
  return new Promise((resolve) => {
    const child = execFile(
      quiet ? TASKPOLICY : "du",
      quiet ? ["-c", "utility", "du", ...du] : du,
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
      // `taskpolicy` becomes `du` (it execs), so this is the walker's own priority either way
      if (child.pid !== undefined) os.setPriority(child.pid, 19);
    } catch {
      // it runs at the priority it has
    }
    onChild?.(child);
  });
}

/**
 * @param {{ file?: string | null, ttlMs?: number, now?: () => number, platform?: string,
 *   measure?: (folder: string, options: { onChild: (child: import("node:child_process").ChildProcess) => void }) => Promise<number | null>,
 *   isWorktree?: (folder: string) => boolean, rest?: (ms: number) => Promise<void> }} [options]
 */
export function createTreeSizes({
  file = null,
  ttlMs = TREE_SIZE_TTL_MS,
  now = Date.now,
  platform = process.platform,
  measure = measureWithDu,
  isWorktree = (folder) => path.isAbsolute(folder) && existsSync(path.join(folder, ".git")),
  rest = (ms) => new Promise((resolve) => { setTimeout(resolve, ms).unref?.(); }),
} = {}) {
  /** @type {Map<string, { bytes: number | undefined, at: number }>} */
  const known = new Map();
  /** @type {string[]} */
  let queue = [];
  let running = null;
  let child = null;
  let saveTimer = null;
  let dirty = false;
  let disposed = false;

  if (file !== null) {
    try {
      const kept = JSON.parse(readFileSync(file, "utf8"));
      for (const [folder, entry] of Object.entries(kept?.sizes ?? {})) {
        if (
          path.isAbsolute(folder) && Number.isSafeInteger(entry?.bytes) && entry.bytes >= 0 &&
          typeof entry.at === "number" && Number.isFinite(entry.at)
        ) known.set(folder, { bytes: entry.bytes, at: entry.at });
        if (known.size >= KEPT) break;
      }
    } catch {
      // no file yet, or one this reader cannot use: figures are measured again as they are asked for
    }
  }

  const save = () => {
    if (file === null || !dirty) return;
    dirty = false;
    const sizes = {};
    for (const [folder, entry] of known) if (entry.bytes !== undefined) sizes[folder] = entry;
    try {
      mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
      writeFileSync(`${file}.tmp`, JSON.stringify({ v: 1, sizes }), { mode: 0o600 });
      renameSync(`${file}.tmp`, file);
    } catch {
      dirty = true; // the figures stand in memory; the next walk tries the file again
    }
  };
  const saveSoon = () => {
    dirty = true;
    if (file === null || saveTimer !== null) return;
    saveTimer = setTimeout(() => { saveTimer = null; save(); }, SAVE_DELAY_MS);
    saveTimer.unref?.();
  };

  const next = () => {
    if (running !== null || disposed) return;
    const folder = queue.shift();
    if (folder === undefined) return;
    running = folder;
    const started = now();
    const done = (bytes) => {
      child = null;
      if (disposed) return;
      known.delete(folder);
      known.set(folder, { bytes: bytes ?? undefined, at: now() });
      while (known.size > KEPT) known.delete(known.keys().next().value);
      if (bytes !== null) saveSoon();
      const pause = Math.max(TREE_SIZE_REST_MIN_MS, Math.max(0, now() - started) * TREE_SIZE_REST_FACTOR);
      // the figure is answerable at once; only the NEXT walk waits out the rest
      void rest(pause).then(() => { running = null; next(); });
    };
    Promise.resolve()
      .then(() => (isWorktree(folder) ? measure(folder, { onChild: (started) => { child = started; } }) : null))
      .then(done, () => done(null));
  };

  return {
    /** A standing figure, however old; undefined when there is none. Queues nothing. */
    peek(folder) {
      return known.get(folder)?.bytes;
    },
    /** The figures for these folders, in their order, and the walks they are
     * owed. Per folder: bytes (a fresh figure, or an old one while its
     * replacement is walked); null while a first walk is queued or running;
     * undefined when this host does not measure (Windows has no `du`) or the
     * folder could not be read. The queue becomes exactly what THIS ask is
     * owed: what an earlier ask queued and this one does not name is dropped. */
    ask(folders) {
      if (platform === "win32" || disposed) return folders.map(() => undefined);
      const at = now();
      const owed = [];
      const answers = folders.map((folder, index) => {
        if (index >= TREE_SIZE_ASK_MAX) return undefined;
        const entry = known.get(folder);
        if (entry !== undefined) {
          const life = entry.bytes === undefined ? TREE_SIZE_FAILED_TTL_MS : ttlMs;
          if (at - entry.at < life) return entry.bytes;
        }
        if (!owed.includes(folder)) owed.push(folder);
        return entry?.bytes ?? null;
      });
      queue = owed.filter((folder) => folder !== running);
      next();
      return answers;
    },
    /** Stop: the walk in flight is ended, nothing further starts, and what is known is written. */
    dispose() {
      disposed = true;
      queue = [];
      child?.kill();
      if (saveTimer !== null) clearTimeout(saveTimer);
      saveTimer = null;
      save();
    },
  };
}
