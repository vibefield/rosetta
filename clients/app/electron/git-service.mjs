// Git, in the process that already owns the worktree root.
//
// It runs HERE — the mille UtilityProcess — rather than in the main process
// or the renderer, for three reasons: this process is already forked per
// project root and dies with it, mille's own git clients are built to live
// here (`@vibecook/mille-ui/git/node` is Node-only by design and the
// decoration provider is already registered in this file's neighbour), and
// the renderer stays a process that cannot reach a filesystem at all.
//
// Every path that reaches a git invocation goes through mille's
// `assertPathUnderRoot`, which rejects `..`, absolute paths and drive
// letters. The root itself is fixed at fork time and is never a parameter.

import { execFile } from "node:child_process";
import { assertPathUnderRoot, createShellScmClient } from "@vibecook/mille-ui/git/node";

/** Long enough for a cold `git log` on a large repository, short enough that
 * a wedged git never becomes a wedged column. */
const TIMEOUT_MS = 10_000;
/** A diff big enough to be worth refusing before it crosses the port. */
const MAX_DIFF_BYTES = 8 * 1024 * 1024;
/** How many commits the History view asks for. The column is a reader, not
 * an archive: a reader who needs the four-hundredth commit wants a terminal. */
const LOG_LIMIT = 200;

function git(root, args, options = {}) {
  return new Promise((resolve) => {
    execFile(
      "git",
      ["-C", root, ...args],
      { timeout: TIMEOUT_MS, maxBuffer: options.maxBuffer ?? MAX_DIFF_BYTES },
      (error, stdout, stderr) => {
        resolve({
          ok: !error,
          stdout: stdout ?? "",
          stderr: (stderr ?? "").trim() || (error ? String(error.message) : ""),
        });
      },
    );
  });
}

/** `.` means "unchanged in this slot" — every other character is a state. */
function slot(letter) {
  return letter === "." || letter === undefined ? null : letter;
}

/**
 * `git status --porcelain=v2 -z`, keeping BOTH slots.
 *
 * mille's own client collapses the (index, worktree) pair into one letter,
 * which is right for a row decoration and wrong here: a file with staged
 * changes AND further unstaged edits is genuinely in both groups, and
 * collapsing it would hide half of what the reader is about to commit.
 */
function parseStatus(raw) {
  const records = raw.split("\0");
  const entries = [];

  for (let i = 0; i < records.length; i += 1) {
    const line = records[i];
    if (line === "") continue;
    const tag = line[0];

    if (tag === "1" || tag === "2") {
      const xy = line.slice(2, 4);
      // `1 XY sub mH mI mW hH hI path` — the path is the 9th field, and it
      // may itself contain spaces, so it is taken as the remainder.
      const fields = line.split(" ");
      const skip = tag === "1" ? 8 : 9;
      const path = fields.slice(skip).join(" ");
      // A rename's original path is the NEXT NUL-separated record.
      let original = null;
      if (tag === "2") {
        original = records[i + 1] ?? null;
        i += 1;
      }
      entries.push({
        path,
        original,
        index: slot(xy[0]),
        worktree: slot(xy[1]),
      });
    } else if (tag === "?") {
      // Untracked. git says nothing about an index slot because there is
      // none, and the room should not invent an `A` it has not earned.
      entries.push({ path: line.slice(2), original: null, index: null, worktree: "?" });
    } else if (tag === "u") {
      // Unmerged. A conflict is one state, not two slots.
      const fields = line.split(" ");
      entries.push({
        path: fields.slice(10).join(" "),
        original: null,
        index: null,
        worktree: "U",
      });
    }
    // `!` ignored files are not asked for, and `#` headers are not entries.
  }
  return entries;
}

/** `sha ⟂ subject ⟂ author ⟂ unix ⟂ parents ⟂ co-authors` per commit.
 *
 * The co-authors come from git's OWN trailer parser rather than a regex over
 * the body: `Co-authored-by` is case-insensitive, may repeat, and folds
 * continuation lines — all of which git already knows and a hand-rolled
 * match gets wrong on somebody else's commit. */
const LOG_FORMAT =
  "%H%x1f%s%x1f%an%x1f%at%x1f%P%x1f%(trailers:key=Co-authored-by,valueonly,separator=%x1e)";

function parseLog(raw, unpushed) {
  const out = [];
  for (const line of raw.split("\n")) {
    if (line.trim() === "") continue;
    const [sha, subject, author, at, parents, coAuthored] = line.split("\x1f");
    if (!sha) continue;
    out.push({
      sha,
      short: sha.slice(0, 7),
      subject: subject ?? "",
      author: author ?? "",
      timestampMs: Number.parseInt(at ?? "0", 10) * 1000,
      // A merge has more than one parent, and its diffstat is not a fact
      // about one person's work.
      merge: (parents ?? "").trim().split(/\s+/).filter(Boolean).length > 1,
      // `Name <email>` per trailer. The name is what a byline shows; the
      // address is kept because two agents can share a display name and the
      // reader may need to tell them apart.
      coAuthors: (coAuthored ?? "")
        .split("\x1e")
        .map((entry) => entry.trim())
        .filter(Boolean)
        .map((entry) => {
          const angle = entry.lastIndexOf("<");
          return angle === -1
            ? { name: entry, email: null }
            : {
                name: entry.slice(0, angle).trim(),
                email: entry.slice(angle + 1).replace(">", "").trim() || null,
              };
        }),
      // `null` is not `false`: a branch with no upstream cannot answer this,
      // and the row draws no mark rather than claiming the commit is unpushed.
      pushed: unpushed === null ? null : !unpushed.has(sha),
      added: 0,
      deleted: 0,
      files: 0,
    });
  }
  return out;
}

/** `--shortstat` per commit is one process per commit; `--numstat` over the
 * whole range is one process total, and the totals are git's own. */
function applyNumstat(commits, raw) {
  const bySha = new Map(commits.map((c) => [c.sha, c]));
  let current = null;
  for (const line of raw.split("\n")) {
    if (line === "") continue;
    if (!line.includes("\t")) {
      current = bySha.get(line.trim()) ?? null;
      continue;
    }
    if (current === null) continue;
    const [added, deleted] = line.split("\t");
    current.files += 1;
    // `-` is git's way of saying "binary", and it is not a line count.
    if (added !== "-") current.added += Number.parseInt(added, 10) || 0;
    if (deleted !== "-") current.deleted += Number.parseInt(deleted, 10) || 0;
  }
}

export function createGitService(root) {
  const scm = createShellScmClient({ rootPath: root });

  /** Reject anything that is not a path inside this root, before git sees it. */
  const safe = (paths) => {
    if (!Array.isArray(paths) || paths.length === 0) {
      throw new Error("no paths given");
    }
    return paths.map((p) => assertPathUnderRoot(root, p));
  };

  return {
    /** Branch, upstream, and how far apart they are. Every field is
     * nullable: a detached HEAD has no branch, and a branch with no
     * upstream has no counts. */
    async branch() {
      const head = await git(root, ["rev-parse", "--abbrev-ref", "HEAD"]);
      const name = head.ok ? head.stdout.trim() : null;
      const upstream = await git(root, [
        "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}",
      ]);
      if (!upstream.ok) {
        return { name, upstream: null, ahead: null, behind: null };
      }
      const counts = await git(root, ["rev-list", "--left-right", "--count", "@{u}...HEAD"]);
      const [behind, ahead] = counts.ok
        ? counts.stdout.trim().split(/\s+/).map((n) => Number.parseInt(n, 10))
        : [null, null];
      return {
        name,
        upstream: upstream.stdout.trim(),
        ahead: Number.isFinite(ahead) ? ahead : null,
        behind: Number.isFinite(behind) ? behind : null,
      };
    },

    async status() {
      const result = await git(root, [
        "status", "--porcelain=v2", "-z", "--untracked-files=all",
      ]);
      if (!result.ok) return { entries: [], error: result.stderr };
      return { entries: parseStatus(result.stdout), error: null };
    },

    async log() {
      const unpushedResult = await git(root, ["rev-list", "@{u}..HEAD"]);
      const unpushed = unpushedResult.ok
        ? new Set(unpushedResult.stdout.split("\n").map((s) => s.trim()).filter(Boolean))
        : null;

      const result = await git(root, [
        "log", `--max-count=${LOG_LIMIT}`, `--format=${LOG_FORMAT}`,
      ]);
      if (!result.ok) return { commits: [], error: result.stderr };
      const commits = parseLog(result.stdout, unpushed);

      const stat = await git(root, [
        "log", `--max-count=${LOG_LIMIT}`, "--format=%H", "--numstat", "--no-renames",
      ]);
      if (stat.ok) applyNumstat(commits, stat.stdout);
      return { commits, error: null };
    },

    /**
     * One file's unified diff. `staged` picks the index against HEAD rather
     * than the working tree against the index — the two halves the Changes
     * view is already showing as two groups.
     */
    async diff({ path: relative, staged, sha }) {
      const [only] = safe([relative]);
      const args = sha
        ? ["show", "--format=", "-M", `${sha}`, "--", only]
        : staged
          ? ["diff", "--cached", "-M", "--", only]
          : ["diff", "-M", "--", only];
      const result = await git(root, args);
      if (!result.ok) return { text: "", error: result.stderr };
      // An untracked file has no diff at all; `--no-index` against /dev/null
      // is how git will show one, and it exits 1 by design when they differ.
      if (result.stdout === "" && !staged && !sha) {
        const untracked = await git(root, [
          "diff", "--no-index", "--", "/dev/null", only,
        ]);
        return { text: untracked.stdout, error: null };
      }
      return { text: result.stdout, error: null };
    },

    /**
     * Which files one commit touched, with the letter each earned.
     *
     * Two passes because git will not emit both in one invocation:
     * `--name-status` knows an add from a rename, `--numstat` knows the line
     * counts. The letter is the half a reader scans by, so it leads.
     */
    async commitFiles(sha) {
      const [status, stat] = await Promise.all([
        git(root, ["show", "--format=", "--name-status", "-M", sha]),
        git(root, ["show", "--format=", "--numstat", "-M", sha]),
      ]);
      if (!status.ok) return { files: [], error: status.stderr };

      const counts = new Map();
      for (const line of stat.stdout.split("\n")) {
        if (!line.includes("\t")) continue;
        const parts = line.split("\t");
        const [added, deleted] = parts;
        // A rename's numstat row carries three path fields; the last is the
        // destination, which is the path the letter pass will also name.
        const path = parts[parts.length - 1];
        counts.set(path, {
          added: added === "-" ? null : Number.parseInt(added, 10) || 0,
          deleted: deleted === "-" ? null : Number.parseInt(deleted, 10) || 0,
        });
      }

      const files = [];
      for (const line of status.stdout.split("\n")) {
        if (!line.includes("\t")) continue;
        const parts = line.split("\t");
        // `R100 old new` for a rename or copy; `M path` for everything else.
        const letter = parts[0][0];
        const original = parts.length > 2 ? parts[1] : null;
        const path = parts[parts.length - 1];
        files.push({
          path,
          original,
          status: letter,
          ...(counts.get(path) ?? { added: null, deleted: null }),
        });
      }
      return { files, error: null };
    },

    async stage(paths) {
      await scm.stage(safe(paths));
    },

    async unstage(paths) {
      await scm.unstage(safe(paths));
    },

    /**
     * Throw a file's changes away.
     *
     * mille's `revert` restores the working tree, which is the whole answer
     * for a tracked file — but an UNTRACKED file has nothing to restore to,
     * and `git restore` silently succeeds while leaving it on disk. Those
     * are removed explicitly, which is what "discard" means to the person
     * who pressed it.
     */
    async discard(paths) {
      const checked = safe(paths);
      const status = await git(root, ["status", "--porcelain=v2", "-z", "--untracked-files=all"]);
      const untracked = new Set(
        status.ok
          ? parseStatus(status.stdout).filter((e) => e.worktree === "?").map((e) => e.path)
          : [],
      );
      const tracked = checked.filter((p) => !untracked.has(p));
      const newFiles = checked.filter((p) => untracked.has(p));
      if (tracked.length > 0) {
        // Unstage first: `git restore --worktree` leaves a staged change in
        // the index, and a file that came back on the next status read is
        // not what "discard" promised.
        await git(root, ["restore", "--staged", "--", ...tracked]);
        await scm.revert(tracked);
      }
      if (newFiles.length > 0) {
        const removed = await git(root, ["clean", "-f", "--", ...newFiles]);
        if (!removed.ok) throw new Error(removed.stderr || "could not remove untracked files");
      }
    },
  };
}
