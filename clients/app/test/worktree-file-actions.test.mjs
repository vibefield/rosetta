import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { createWorktreeFileActionHandler } from "../electron/worktree-file-actions.mjs";

function fixture(t, overrides = {}) {
  const root = mkdtempSync(path.join(tmpdir(), "rosetta-file-actions-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(path.join(root, "nested"));
  writeFileSync(path.join(root, "nested", "file ü.txt"), "fixture");
  const contents = { mainFrame: {} };
  const event = { sender: contents, senderFrame: contents.mainFrame };
  let session = { root, generation: 7, contents, ready: true, stopping: false };
  const copied = [];
  const revealed = [];
  const handler = createWorktreeFileActionHandler({
    getSession: () => session,
    writeClipboard: (text) => copied.push(text),
    revealInFileManager: (target) => revealed.push(target),
    ...overrides,
  });
  const request = { root, generation: 7, action: "copyPath", paths: ["nested/file ü.txt"] };
  return { root, contents, event, request, copied, revealed, handler, setSession: (next) => { session = next; } };
}

test("copies absolute paths in one clipboard write, including directories and the root", async (t) => {
  const f = fixture(t);
  assert.deepEqual(await f.handler(f.event, { ...f.request, paths: ["nested/file ü.txt", "nested", ""] }), { ok: true });
  assert.deepEqual(f.copied, [[path.join(f.root, "nested/file ü.txt"), path.join(f.root, "nested"), f.root].join("\n")]);
  assert.deepEqual(f.revealed, []);
});

test("reveals the exact entry, including a symlink, and reports missing files", async (t) => {
  const f = fixture(t);
  symlinkSync(path.join(f.root, "missing-target"), path.join(f.root, "link"));
  for (const relativePath of ["nested/file ü.txt", "nested", "link", ""]) {
    assert.deepEqual(await f.handler(f.event, { ...f.request, action: "revealInFileManager", paths: [relativePath] }), { ok: true });
  }
  assert.deepEqual(f.revealed, ["nested/file ü.txt", "nested", "link", ""].map((p) => path.resolve(f.root, p)));
  const missing = await f.handler(f.event, { ...f.request, action: "revealInFileManager", paths: ["gone"] });
  assert.equal(missing.ok, false);
  assert.match(missing.error, /no longer available/);
  assert.equal(f.revealed.length, 4);
});

test("rejects stale generations, other worktrees, other windows, and subframes", async (t) => {
  const f = fixture(t);
  for (const [event, request] of [
    [f.event, { ...f.request, generation: 6 }],
    [f.event, { ...f.request, generation: "7" }],
    [f.event, { ...f.request, root: `${f.root}-other` }],
    [{ ...f.event, sender: {} }, f.request],
    [{ ...f.event, senderFrame: {} }, f.request],
    [{ ...f.event, senderFrame: null }, f.request],
  ]) assert.equal((await f.handler(event, request)).ok, false);
  for (const session of [null, { root: f.root, generation: 7, contents: f.contents, ready: false },
    { root: f.root, generation: 7, contents: f.contents, ready: true, stopping: true }]) {
    f.setSession(session);
    assert.equal((await f.handler(f.event, f.request)).ok, false);
  }
  assert.deepEqual(f.copied, []);
  assert.deepEqual(f.revealed, []);
});

test("rejects malformed paths and validates the entire batch before touching the OS", async (t) => {
  const f = fixture(t);
  for (const invalid of ["../outside", "nested/../../outside", "/etc/passwd", "C:/outside", "C:outside", "\\\\server\\share", "nested\\file", "nested/./file", "nested//file", "nested/", "bad\0name", null, 3]) {
    assert.equal((await f.handler(f.event, { ...f.request, paths: ["nested/file ü.txt", invalid] })).ok, false);
  }
  for (const request of [null, {}, { ...f.request, action: "delete" }, { ...f.request, paths: [] },
    { ...f.request, paths: "nested" }, { ...f.request, paths: Array(10_001).fill("nested") },
    { ...f.request, paths: ["x".repeat(1024 * 1024)] },
    { ...f.request, action: "revealInFileManager", paths: ["nested", ""] }]) {
    assert.equal((await f.handler(f.event, request)).ok, false);
  }
  assert.deepEqual(f.copied, []);
  assert.deepEqual(f.revealed, []);
});

test("returns clipboard and shell failures to the renderer", async (t) => {
  const f = fixture(t, {
    writeClipboard: async () => { throw new Error("Clipboard unavailable"); },
    revealInFileManager: () => { throw new Error("File manager unavailable"); },
  });
  assert.deepEqual(await f.handler(f.event, f.request), { ok: false, error: "Clipboard unavailable" });
  assert.deepEqual(await f.handler(f.event, { ...f.request, action: "revealInFileManager" }), { ok: false, error: "File manager unavailable" });
});
