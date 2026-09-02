// Rosetta's product policy for the Mille worktree host. Kept free of Electron
// imports so the exact production configuration can be exercised by the Node
// integration gate without starting a window or a utility process.

export const WORKTREE_DIRECTORY_BATCH_SIZE = 256;

export function createWorktreeExplorerOptions(root) {
  return {
    roots: [root],
    respectIgnore: true,
    followSymlinks: "smart",
    watchDebounceMs: 75,
    // Seed only the root. Expanding a directory publishes a bounded first
    // page, then geometrically larger pages; the tree remains usable while a
    // pathological directory finishes, and collapsing its last interested
    // view cancels the native read. A history room must never walk a monorepo
    // upfront or wait for a wide directory before drawing its first rows.
    initialWalk: "roots-only",
    // Pin the product's latency budget rather than inheriting a library
    // default that may evolve. 256 is Mille's measured interactive default:
    // enough rows to cover the rail and its overscan in the first page, while
    // later adaptive pages amortize immutable-snapshot publication costs.
    directoryBatchSize: WORKTREE_DIRECTORY_BATCH_SIZE,
    // One row is one directory. Mille's default collapses a single-child
    // chain into `clients/app/electron`, which is an IDE convenience that
    // trades a path for a name — and the design record's rail (§8) names one
    // folder per row. It is also the honest reading under lazy expansion:
    // "only one child" is a claim about what has been walked, not about the
    // directory.
    compactFolders: false,
  };
}
