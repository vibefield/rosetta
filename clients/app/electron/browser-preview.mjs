// Materialize one viewed HTML or SVG document for the user's default browser.
//
// The renderer may have read the bytes from a worktree, a transcript
// attachment, or a canonical timeline resource. Some of those have no local
// path, and a path-backed resource may change between "read" and "open". The
// browser therefore gets a private snapshot of the bytes the reader actually
// saw, never a renderer-selected path.

import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";

export const MAX_BROWSER_PREVIEW_BYTES = 2 * 1024 * 1024;

const ALLOWED_EXTENSIONS = new Set([".htm", ".html", ".svg"]);

function extensionOf(name) {
  if (typeof name !== "string" || name.length === 0 || name.length > 4096) return null;
  const extension = path.extname(name).toLowerCase();
  return ALLOWED_EXTENSIONS.has(extension) ? extension : null;
}

function bytesOf(raw) {
  if (!(raw instanceof Uint8Array) || raw.byteLength > MAX_BROWSER_PREVIEW_BYTES) {
    return null;
  }
  // Own the memory before hashing it. IPC normally gives main its own clone,
  // but this also keeps the service correct when called directly in tests or
  // by another main-process caller later.
  return Buffer.from(raw);
}

/**
 * Create an app-session-scoped browser preview service.
 *
 * `openPath` is injected so the file boundary is testable without launching
 * anybody's browser. It follows Electron's `shell.openPath` contract: an
 * empty string is success and any other string is the reason it failed.
 */
export function createBrowserPreviewService({ tempDirectory, openPath }) {
  if (typeof tempDirectory !== "string" || tempDirectory.length === 0) {
    throw new TypeError("browser preview needs a temporary directory");
  }
  if (typeof openPath !== "function") {
    throw new TypeError("browser preview needs an openPath function");
  }

  let sessionDirectory = null;

  function ensureSessionDirectory() {
    if (sessionDirectory === null) {
      sessionDirectory = mkdtempSync(path.join(tempDirectory, "rosetta-browser-preview-"));
    }
    return sessionDirectory;
  }

  async function open(raw) {
    if (raw === null || typeof raw !== "object") return false;
    const extension = extensionOf(raw.name);
    const bytes = bytesOf(raw.bytes);
    if (extension === null || bytes === null) return false;

    // The renderer controls neither the directory nor the stem. A digest
    // deduplicates repeated opens while the extension tells the OS which safe
    // default handler to use.
    const digest = createHash("sha256").update(bytes).digest("hex");
    const target = path.join(ensureSessionDirectory(), `${digest}${extension}`);
    try {
      writeFileSync(target, bytes, { flag: "wx", mode: 0o600 });
    } catch (error) {
      if (error?.code !== "EEXIST") return false;
      // This should only be our own earlier snapshot. Verify rather than
      // assuming, so an unexpected collision or mutation never opens bytes
      // other than the ones requested.
      try {
        if (!readFileSync(target).equals(bytes)) return false;
      } catch {
        return false;
      }
    }

    try {
      return (await openPath(target)) === "";
    } catch {
      return false;
    }
  }

  function dispose() {
    const directory = sessionDirectory;
    sessionDirectory = null;
    if (directory === null) return;
    try {
      rmSync(directory, { recursive: true, force: true });
    } catch {
      // Temporary previews are expendable. A failed best-effort cleanup must
      // not hold the app open; the OS can reap its own temp directory later.
    }
  }

  return { open, dispose };
}
