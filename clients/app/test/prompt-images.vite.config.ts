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
    const reads: Record<string, number> = {};
    server.middlewares.use((request, response, next) => {
      if (request.url === "/test/image-reads") {
        response.setHeader("content-type", "application/json"); response.end(JSON.stringify(reads)); return;
      }
      const url = new URL(request.url ?? "/", "http://fixture");
      const variant = url.searchParams.get("image") ?? "original";
      if (url.pathname.startsWith("/test/fixtures/prompt-images/") && variant !== "original") {
        if (url.searchParams.get("fail") === "read") { response.statusCode = 503; response.end("Temporary fixture failure"); return; }
        if (url.searchParams.get("fail") === "decode") { response.setHeader("content-type", "image/png"); response.end("invalid image"); return; }
        if (url.searchParams.has("gallery-slow")) {
          setTimeout(() => { response.setHeader("content-type", "image/png"); response.end(readFileSync(new URL("./fixtures/prompt-images/portrait.png", import.meta.url))); }, 450);
          return;
        }
      }
      const target = /^\/api\/sessions\/s1\/timeline\/(image-read|image-artifact)\/read\/(\d+)$/.exec(new URL(request.url ?? "/", "http://fixture").pathname);
      if (target) {
        const key = `${target[1]}:${target[2]}`;
        reads[key] = (reads[key] ?? 0) + 1;
        reads[`${key}:${variant}`] = (reads[`${key}:${variant}`] ?? 0) + 1;
        if (target[1] === "image-read" && target[2] === "0") { response.statusCode = 400; response.end("Not an image"); return; }
        if (key === "image-read:3" && reads[key] === 1) { response.statusCode = 503; response.end("Temporary fixture failure"); return; }
        response.setHeader("content-type", "application/octet-stream");
        response.setHeader("x-rosetta-protocol-version", String(PROTOCOL_VERSION));
        response.end(readFileSync(new URL(`./fixtures/prompt-images/${Number(target[2]) % 2 ? "landscape" : "portrait"}.png`, import.meta.url)));
        return;
      }
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
