import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import {
  MAX_BROWSER_PREVIEW_BYTES,
  createBrowserPreviewService,
} from "../electron/browser-preview.mjs";

function fixture(openPath = async () => "") {
  const parent = mkdtempSync(path.join(tmpdir(), "rosetta-preview-test-"));
  const opened = [];
  const service = createBrowserPreviewService({
    tempDirectory: parent,
    openPath: async (target) => {
      opened.push(target);
      return openPath(target);
    },
  });
  return {
    parent,
    opened,
    service,
    dispose() {
      service.dispose();
      rmSync(parent, { recursive: true, force: true });
    },
  };
}

test("opens an exact private snapshot, independent of the supplied path", async () => {
  const scope = fixture();
  try {
    const bytes = new TextEncoder().encode("<!doctype html><title>artifact</title>");
    assert.equal(
      await scope.service.open({ name: "../../somewhere/report.HTML", bytes }),
      true,
    );

    assert.equal(scope.opened.length, 1);
    const target = scope.opened[0];
    assert.equal(path.dirname(target).startsWith(`${scope.parent}${path.sep}`), true);
    assert.match(path.basename(target), /^[a-f0-9]{64}\.html$/);
    assert.deepEqual(readFileSync(target), Buffer.from(bytes));
    assert.equal(statSync(target).mode & 0o777, 0o600);
  } finally {
    scope.dispose();
  }
});

test("supports pathless HTML, HTM, SVG, and empty documents", async () => {
  const scope = fixture();
  try {
    for (const name of ["artifact.html", "old-page.htm", "drawing.svg"]) {
      assert.equal(await scope.service.open({ name, bytes: new Uint8Array(0) }), true);
    }
    assert.deepEqual(
      scope.opened.map((target) => path.extname(target)),
      [".html", ".htm", ".svg"],
    );
  } finally {
    scope.dispose();
  }
});

test("refuses malformed, non-browser, and oversized requests before writing", async () => {
  const scope = fixture();
  try {
    const oversized = new Uint8Array(MAX_BROWSER_PREVIEW_BYTES + 1);
    for (const request of [
      null,
      {},
      { name: "page.txt", bytes: new Uint8Array([1]) },
      { name: "page.html", bytes: [1] },
      { name: "page.html", bytes: oversized },
      { name: `${"a".repeat(4096)}.html`, bytes: new Uint8Array(0) },
    ]) {
      assert.equal(await scope.service.open(request), false);
    }
    assert.deepEqual(scope.opened, []);
    assert.equal(existsSync(scope.parent), true);
  } finally {
    scope.dispose();
  }
});

test("deduplicates identical bytes and reports shell refusal or failure", async () => {
  const scope = fixture(async (_target) => (scope.opened.length === 1 ? "" : "no handler"));
  try {
    const request = {
      name: "page.html",
      bytes: new TextEncoder().encode("<p>same bytes</p>"),
    };
    assert.equal(await scope.service.open(request), true);
    assert.equal(await scope.service.open(request), false);
    assert.equal(scope.opened[0], scope.opened[1]);
  } finally {
    scope.dispose();
  }

  const throwing = fixture(async () => {
    throw new Error("open failed");
  });
  try {
    assert.equal(
      await throwing.service.open({ name: "page.html", bytes: new Uint8Array(0) }),
      false,
    );
  } finally {
    throwing.dispose();
  }
});

test("dispose removes only this service's session directory", async () => {
  const scope = fixture();
  try {
    const neighbor = path.join(scope.parent, "keep-me");
    writeFileSync(neighbor, "still here");
    assert.equal(
      await scope.service.open({ name: "page.html", bytes: new Uint8Array([60, 62]) }),
      true,
    );
    const sessionDirectory = path.dirname(scope.opened[0]);
    assert.equal(existsSync(sessionDirectory), true);

    scope.service.dispose();
    assert.equal(existsSync(sessionDirectory), false);
    assert.equal(readFileSync(neighbor, "utf8"), "still here");
  } finally {
    scope.dispose();
  }
});
