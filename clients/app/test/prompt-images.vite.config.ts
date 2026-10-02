import { readFileSync } from "node:fs";
import { mergeConfig } from "vite";
import base from "../vite.config.ts";
import { PROTOCOL_VERSION } from "../../ts/protocol-version.mjs";
import { memberText } from "./prompt-images-data.mjs";

// Production-encoded immutable members; no current-file image reads exist here.
export default mergeConfig(base, {
  root: ".",
  plugins: [{ name: "image-retry-fixture", configureServer(server) {
    const failed = new Set<string>();
    const reads: Record<string, number> = {};
    server.middlewares.use((request, response, next) => {
      if (request.url === "/test/image-reads") {
        response.setHeader("content-type", "application/json"); response.end(JSON.stringify(reads)); return;
      }
      const url = new URL(request.url ?? "/", "http://fixture");
      if (/\/timeline\/.*\/read\//.test(url.pathname)) {
        reads.liveFile = (reads.liveFile ?? 0) + 1; response.statusCode = 404; response.end(); return;
      }
      const match = /^\/api\/assets\/([a-f0-9]{64}\.webp)$/.exec(url.pathname);
      if (match) {
        const name = url.searchParams.get("fixture") ?? "asset";
        const variant = url.searchParams.get("variant") ?? "original";
        reads[`${name}:${variant}`] = (reads[`${name}:${variant}`] ?? 0) + 1;
        if (url.searchParams.get("fail") === "read" || (["retry", "tool-retry"].includes(name) && !failed.has(name))) {
          failed.add(name); response.statusCode = 503; response.end("Temporary fixture failure"); return;
        }
        response.setHeader("content-type", "image/webp");
        if (url.searchParams.get("fail") === "decode") { response.end("invalid image"); return; }
        const bytes = readFileSync(new URL(`./fixtures/prompt-images/optimized/${match[1]}`, import.meta.url));
        if (name === "gallery-slow") setTimeout(() => response.end(bytes), 450);
        else response.end(bytes);
        return;
      }
      if (url.pathname === "/test/fixtures/prompt-images/full-message.md") {
        response.setHeader("content-type", "text/plain; charset=utf-8"); response.end(memberText); return;
      }
      next();
    });
  } }],
  server: { hmr: false, watch: null, host: "127.0.0.1", port: 5178,
    headers: { "x-rosetta-protocol-version": String(PROTOCOL_VERSION) } },
});
