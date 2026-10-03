import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The renderer is a plain Vite app; the Electron main process is plain ESM
// with no build step. The protocol SDK is aliased straight to its TypeScript
// source in clients/ts, so there is exactly one copy of the wire types, and
// the History room straight to its package's source in clients/room (its
// three exports, matched exactly). The room's own dependencies resolve from
// clients/room's install; react is deduped to this app's single copy.
//
// @vibecook/mille + mille-ui are file: symlinks into the local mille
// checkout (the published 0.3.3 predates the vibefield theme). Symlinked
// packages resolve bare imports from their REAL path, which would hand
// mille-ui the checkout's own React — dedupe pins react and the engine to
// this app's single copies, and fs.allow lets dev serve the checkout.
export default defineConfig({
  root: "renderer",
  base: "./",
  plugins: [react()],
  resolve: {
    alias: [
      { find: /^@rosetta\/protocol$/, replacement: fileURLToPath(new URL("../ts/index.ts", import.meta.url)) },
      { find: /^@rosetta\/room$/, replacement: fileURLToPath(new URL("../room/src/index.ts", import.meta.url)) },
      { find: /^@rosetta\/room\/fixtures$/, replacement: fileURLToPath(new URL("../room/src/fixtures/index.ts", import.meta.url)) },
      {
        find: /^@rosetta\/room\/styles\/room\.css$/,
        replacement: fileURLToPath(new URL("../room/src/styles/history-room.css", import.meta.url)),
      },
    ],
    dedupe: ["react", "react-dom", "@vibecook/mille"],
  },
  server: {
    port: 5185,
    strictPort: true,
    fs: {
      allow: [
        fileURLToPath(new URL(".", import.meta.url)),
        fileURLToPath(new URL("../ts", import.meta.url)),
        fileURLToPath(new URL("../room", import.meta.url)),
        fileURLToPath(new URL("../../../mille", import.meta.url)),
      ],
    },
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
  },
});
