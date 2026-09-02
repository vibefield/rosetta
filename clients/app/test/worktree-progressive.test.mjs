import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { MessageChannel } from "node:worker_threads";
import { createFileExplorerHost } from "@vibecook/mille/host";
import { connectFileExplorer } from "@vibecook/mille/port";
import {
  createWorktreeExplorerOptions,
  WORKTREE_DIRECTORY_BATCH_SIZE,
} from "../electron/worktree-explorer-config.mjs";

const WIDE_DIRECTORY_SIZE = 10_000;

async function createWideDirectory() {
  const root = await mkdtemp(path.join(tmpdir(), "rosetta-mille-progressive-"));
  try {
    for (let offset = 0; offset < WIDE_DIRECTORY_SIZE; offset += 250) {
      const writes = [];
      for (
        let index = offset;
        index < Math.min(WIDE_DIRECTORY_SIZE, offset + 250);
        index += 1
      ) {
        writes.push(
          writeFile(
            path.join(root, `entry-${String(index).padStart(5, "0")}.txt`),
            "",
          ),
        );
      }
      await Promise.all(writes);
    }
    return root;
  } catch (error) {
    await rm(root, { recursive: true, force: true });
    throw error;
  }
}

async function waitFor(predicate, message) {
  const deadline = performance.now() + 20_000;
  while (!predicate()) {
    if (performance.now() > deadline) throw new Error(message);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

test(
  "Rosetta streams a wide worktree directory before its authoritative completion",
  { timeout: 30_000 },
  async () => {
    const rootPath = await createWideDirectory();
    let host;
    let client;
    let attachment;
    let subscription;

    try {
      const options = createWorktreeExplorerOptions(rootPath);
      assert.equal(options.initialWalk, "roots-only");
      assert.equal(options.directoryBatchSize, WORKTREE_DIRECTORY_BATCH_SIZE);

      host = await createFileExplorerHost(options);
      const channel = new MessageChannel();
      attachment = host.attachPort(channel.port1);
      client = await connectFileExplorer(channel.port2, {
        prefetchRows: 200,
        mirrorCap: 20_000,
      });

      const root = client.getSnapshot().roots()[0];
      assert.ok(root, "roots-only handshake must publish the worktree root");
      const expanded = new Set([root.id]);
      let sawLoading = false;
      let sawPartialRows = false;
      let partialHadAuthoritativeCount = false;

      const observe = () => {
        const snapshot = client.getSnapshot();
        const load = snapshot.directoryLoadState(root.id);
        const rows = snapshot.visibleRows({
          expanded,
          offset: 0,
          limit: 500,
          includeIgnored: false,
        });
        const hydratedChildren = Math.max(0, rows.length - 1);
        if (load.state === "loading") sawLoading = true;
        if (load.state === "loading" && hydratedChildren > 0) {
          sawPartialRows = true;
          partialHadAuthoritativeCount ||=
            snapshot.directChildCount(root.id) !== null;
        }
      };

      subscription = client.on("change", observe);
      client.setViewport({ offset: 0, limit: 200, overscan: 0 });
      client.setExpanded({ add: [root.id] });
      observe();

      await waitFor(
        () =>
          client.getSnapshot().directoryLoadState(root.id).state === "complete",
        "progressive worktree load timed out",
      );
      observe();

      assert.equal(
        client.getSnapshot().directChildCount(root.id),
        WIDE_DIRECTORY_SIZE,
      );
      assert.equal(
        sawLoading,
        true,
        "the renderer must observe explicit loading state",
      );
      assert.equal(
        sawPartialRows,
        true,
        "rows must arrive before directory completion",
      );
      assert.equal(
        partialHadAuthoritativeCount,
        false,
        "a partial listing must remain pending until end-of-directory",
      );
    } finally {
      subscription?.dispose();
      attachment?.dispose();
      await client?.dispose();
      await host?.dispose();
      await rm(rootPath, { recursive: true, force: true });
    }
  },
);
