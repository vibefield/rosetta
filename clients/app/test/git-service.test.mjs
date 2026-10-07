import assert from "node:assert/strict";
import { execFile, execFileSync } from "node:child_process";
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { createGitService } from "../electron/git-service.mjs";

/** A repository with two commits: `notes.txt` created, then edited with
 * `todo.md` added beside it. The fixture's own git never runs a hook, a
 * monitor or a signer, whatever the machine's global config says. */
function repository(t) {
  const dir = mkdtempSync(path.join(tmpdir(), "rosetta-git-service-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const root = path.join(dir, "repo");
  execFileSync("git", ["init", "-q", "-b", "main", root]);
  const git = (...args) =>
    execFileSync(
      "git",
      ["-c", "core.fsmonitor=false", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-C", root, ...args],
      { encoding: "utf8" },
    );
  git("config", "user.name", "Fixture");
  git("config", "user.email", "fixture@example.com");
  writeFileSync(path.join(root, "notes.txt"), "one\ntwo\n");
  git("add", ".");
  git("commit", "-qm", "first");
  writeFileSync(path.join(root, "notes.txt"), "one\ntwo\nthree\n");
  writeFileSync(path.join(root, "todo.md"), "- ship\n");
  git("add", ".");
  git("commit", "-qm", "second");
  return { dir, root, sha: git("rev-parse", "HEAD").trim(), git };
}

/** The service over a recorder: every git it starts is real, and its file,
 * argv and environment are kept for the assertions. */
function recorded(root) {
  const calls = [];
  const service = createGitService(root, {
    execFile: (file, args, options, callback) => {
      calls.push({ file, args, env: options.env });
      return execFile(file, args, options, callback);
    },
  });
  return { service, calls };
}

/** A program a repository's config can name, which leaves a mark when run. */
function marker(dir, name, body = "") {
  const script = path.join(dir, `${name}.sh`);
  const mark = path.join(dir, `${name}.ran`);
  writeFileSync(script, `#!/bin/sh\necho ran >> "${mark}"\n${body}`);
  chmodSync(script, 0o755);
  return { script, mark };
}

test("a commit id that is not a hex object id is refused before git runs, and nothing is written", async (t) => {
  const f = repository(t);
  const written = path.join(f.dir, "x");
  const { service, calls } = recorded(f.root);
  const refused = [`--output=${written}`, "-p", "--ext-diff", "", "a".repeat(65), "abc", "HEAD~1", `${f.sha}:notes.txt`];
  for (const sha of refused) {
    const diff = await service.diff({ path: "notes.txt", sha });
    assert.equal(existsSync(written), false, `${sha} wrote a file`);
    assert.equal(diff.text, "", sha);
    assert.match(diff.error, /^not a commit id/, sha);
    const files = await service.commitFiles(sha);
    assert.deepEqual(files.files, [], sha);
    assert.match(files.error, /^not a commit id/, sha);
  }
  for (const sha of [undefined, null, 42, [f.sha]]) {
    const files = await service.commitFiles(sha);
    assert.deepEqual(files.files, []);
    assert.match(files.error, /^not a commit id/);
  }
  assert.deepEqual(calls, []);
  assert.equal(existsSync(written), false);
});

test("a short and a full commit id answer the same diff and file list as before", async (t) => {
  const f = repository(t);
  const { service } = recorded(f.root);
  // What the service answered before the guard: git's own output for the
  // argv it used to build.
  const before = f.git("show", "--format=", "-M", f.sha, "--", "notes.txt");
  assert.match(before, /^\+three$/m);
  for (const sha of [f.sha, f.sha.slice(0, 7)]) {
    assert.deepEqual(await service.diff({ path: "notes.txt", sha }), { text: before, error: null });
    assert.deepEqual(await service.commitFiles(sha), {
      files: [
        { path: "notes.txt", original: null, status: "M", added: 1, deleted: 0 },
        { path: "todo.md", original: null, status: "A", added: 1, deleted: 0 },
      ],
      error: null,
    });
  }
  // A short id that is also a file's name is still the commit.
  writeFileSync(path.join(f.root, f.sha.slice(0, 7)), "");
  assert.equal((await service.commitFiles(f.sha.slice(0, 7))).files.length, 2);
});

test("every git the service starts runs without optional locks or the fsmonitor, and every diff is git's own", async (t) => {
  const f = repository(t);
  writeFileSync(path.join(f.root, "notes.txt"), "one\ntwo\nthree\nfour\n");
  writeFileSync(path.join(f.root, "todo.md"), "- ship\n- land\n");
  f.git("add", "todo.md");
  writeFileSync(path.join(f.root, "scratch.txt"), "loose\n");
  const { service, calls } = recorded(f.root);

  await service.branch();
  await service.status();
  await service.log();
  assert.match((await service.diff({ path: "notes.txt" })).text, /^\+four$/m);
  assert.match((await service.diff({ path: "todo.md", staged: true })).text, /^\+- land$/m);
  assert.match((await service.diff({ path: "scratch.txt" })).text, /^\+loose$/m);
  assert.match((await service.diff({ path: "notes.txt", sha: f.sha })).text, /^\+three$/m);
  assert.equal((await service.commitFiles(f.sha)).files.length, 2);
  await service.discard(["scratch.txt"]);
  assert.equal(existsSync(path.join(f.root, "scratch.txt")), false);

  const ran = new Set(calls.map((call) => call.args[4]));
  for (const command of ["rev-parse", "status", "log", "diff", "show", "clean"]) {
    assert.ok(ran.has(command), `the service ran no git ${command}`);
  }
  for (const { file, args, env } of calls) {
    assert.equal(file, "git");
    assert.deepEqual(args.slice(0, 4), ["-c", "core.fsmonitor=false", "-C", f.root], args.join(" "));
    assert.equal(env?.GIT_OPTIONAL_LOCKS, "0", args.join(" "));
    if (args[4] === "show" || args[4] === "diff" || args.includes("--numstat")) {
      assert.ok(args.includes("--no-ext-diff"), args.join(" "));
      assert.ok(args.includes("--no-textconv"), args.join(" "));
    }
    if (args[4] === "show") {
      // The id is the one value past `--end-of-options` and before `--`.
      const end = args.indexOf("--end-of-options");
      assert.notEqual(end, -1, args.join(" "));
      assert.equal(args[end + 1], f.sha, args.join(" "));
      assert.equal(args[end + 2], "--", args.join(" "));
    }
  }
});

test("a repository's own config cannot make a read run a program", async (t) => {
  const f = repository(t);
  const monitor = marker(f.dir, "fsmonitor", "exit 1\n");
  const external = marker(f.dir, "external");
  const textconv = marker(f.dir, "textconv", 'cat "$1"\n');
  f.git("config", "core.fsmonitor", monitor.script);
  f.git("config", "diff.external", external.script);
  f.git("config", "diff.conv.textconv", textconv.script);
  writeFileSync(path.join(f.root, ".gitattributes"), "*.txt diff=conv\n");
  f.git("add", ".gitattributes");
  f.git("commit", "-qm", "attributes");
  writeFileSync(path.join(f.root, "notes.txt"), "one\ntwo\nthree\nfour\n");
  const { service } = recorded(f.root);

  await service.status();
  await service.log();
  const worktree = await service.diff({ path: "notes.txt" });
  const commit = await service.diff({ path: "notes.txt", sha: f.sha });
  await service.commitFiles(f.sha);
  assert.match(worktree.text, /^diff --git a\/notes\.txt b\/notes\.txt$/m);
  assert.match(worktree.text, /^\+four$/m);
  assert.match(commit.text, /^\+three$/m);
  for (const { mark } of [monitor, external, textconv]) {
    assert.equal(existsSync(mark), false, `${path.basename(mark)} should not exist`);
  }

  // The control: the same reads without the hardening do run all three.
  execFileSync("git", ["-C", f.root, "status"]);
  execFileSync("git", ["-C", f.root, "diff", "--", "notes.txt"]);
  execFileSync("git", ["-C", f.root, "show", f.sha, "--", "notes.txt"]);
  for (const { mark } of [monitor, external, textconv]) {
    assert.equal(existsSync(mark), true, `${path.basename(mark)} should exist`);
  }
});

test("a status read leaves the index as it found it", async (t) => {
  const f = repository(t);
  // Stale the cached stat of a clean file: a status that may take optional
  // locks refreshes it and rewrites the index; one that may not, does not.
  const touch = () => utimesSync(path.join(f.root, "todo.md"), 946_684_800, 946_684_800);
  const index = path.join(f.root, ".git", "index");
  touch();
  const before = readFileSync(index);
  const { service } = recorded(f.root);
  assert.deepEqual((await service.status()).entries, []);
  assert.deepEqual(readFileSync(index), before);

  // The control: git's default does rewrite it.
  const { GIT_OPTIONAL_LOCKS: _, ...env } = process.env;
  execFileSync("git", ["-c", "core.fsmonitor=false", "-C", f.root, "status", "--porcelain"], { env });
  assert.notDeepEqual(readFileSync(index), before);
});
