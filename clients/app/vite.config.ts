import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The renderer is a plain Vite app; the Electron main process is plain ESM
// with no build step. The protocol SDK is aliased straight to its TypeScript
// source in clients/ts, so there is exactly one copy of the wire types.
export default defineConfig({
  root: "renderer",
  base: "./",
  plugins: [react()],
  resolve: {
    alias: {
      "@rosetta/protocol": fileURLToPath(new URL("../ts/index.ts", import.meta.url)),
    },
  },
  server: {
    port: 5185,
    strictPort: true,
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
  },
});
