import { readFileSync } from "node:fs";
import { mergeConfig } from "vite";
import base from "../vite.config.ts";
import { PROTOCOL_VERSION } from "../../ts/protocol-version.mjs";
import { memberText } from "./prompt-images-data.mjs";

// Isolated authored-image fixtures use real asset reads against this server.
export default mergeConfig(base, {
  root: ".",
  plugins: [{ name: "image-retry-fixture", configureServer(server) {
    const failed = new Set<string>();
    server.middlewares.use((request, response, next) => {
      if (request.url?.startsWith("/test/fixtures/prompt-images/full-message.md")) {
        response.setHeader("content-type", "text/plain; charset=utf-8");
        response.setHeader("x-rosetta-protocol-version", String(PROTOCOL_VERSION));
        response.end(memberText);
        return;
      }
      if (!request.url?.startsWith("/test/fixtures/prompt-images/retry.png")) return next();
      const key = new URL(request.url, "http://fixture").searchParams.get("case") ?? "default";
      if (!failed.has(key)) { failed.add(key); response.statusCode = 503; response.end("Temporary fixture failure"); return; }
      response.setHeader("content-type", "image/png");
      response.setHeader("x-rosetta-protocol-version", String(PROTOCOL_VERSION));
      response.end(readFileSync(new URL("./fixtures/prompt-images/landscape.png", import.meta.url)));
    });
  } }],
  server: {
    hmr: false,
    watch: null,
    host: "127.0.0.1",
    port: 5178,
    headers: { "x-rosetta-protocol-version": String(PROTOCOL_VERSION) },
  },
});
