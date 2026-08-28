import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The renderer is a plain Vite app; the Electron main process is plain ESM
// with no build step. The protocol SDK is aliased straight to its TypeScript
// source in clients/ts, so there is exactly one copy of the wire types.
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
    alias: {
      "@rosetta/protocol": fileURLToPath(new URL("../ts/index.ts", import.meta.url)),
    },
    dedupe: ["react", "react-dom", "@vibecook/mille"],
  },
  server: {
    port: 5185,
    strictPort: true,
    fs: {
      allow: [
        fileURLToPath(new URL(".", import.meta.url)),
        fileURLToPath(new URL("../ts", import.meta.url)),
        fileURLToPath(new URL("../../../mille", import.meta.url)),
      ],
    },
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
  },
});
