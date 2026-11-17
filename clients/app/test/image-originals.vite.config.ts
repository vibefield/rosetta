import { readFileSync } from "node:fs";
import { mergeConfig } from "vite";
import base from "../vite.config.ts";
import { PROTOCOL_VERSION } from "../../ts/protocol-version.mjs";
import { ORIGINALS } from "./image-originals-data.mjs";

// A mixed projection's asset route. A legacy bundle's members are served as
// committed. A recorded original answers `?image=<variant>&v=1` with a WebP
// rendition (the portrait bundle's, standing in for the daemon's encode) and
// its plain URL with the recorded bytes, which a display path never asks for.
const fixture = (name: string) => readFileSync(new URL(`./fixtures/prompt-images/${name}`, import.meta.url));
const manifest = JSON.parse(fixture("optimized/manifest.json").toString("utf8"));
const portrait = manifest.portrait.asset;
const RENDITION: Record<string, string> = {
  rail: portrait.image_variants.rail.digest, tile: portrait.image_variants.tile.digest,
  tile2x: portrait.image_variants.tile2x.digest, preview: portrait.image_variants.preview.digest, full: portrait.digest,
};
const original = new Map(Object.entries(ORIGINALS).map(([key, value]) => [value.digest, key]));

export default mergeConfig(base, {
  root: ".",
  plugins: [{ name: "image-originals-fixture", configureServer(server) {
    const reads: Record<string, number> = {};
    const held: (() => void)[] = [];
    server.middlewares.use((request, response, next) => {
      if (request.url === "/test/image-reads") {
        response.setHeader("content-type", "application/json"); response.end(JSON.stringify(reads)); return;
      }
      if (request.url === "/test/release-slow") { held.splice(0).forEach(end => end()); response.end("released"); return; }
      const url = new URL(request.url ?? "/", "http://fixture");
      const match = /^\/api\/assets\/([a-f0-9]{64})\.(png|jpg|gif|webp|bmp|ico)$/.exec(url.pathname);
      if (!match) { next(); return; }
      const [, digest, extension] = match as unknown as [string, string, string];
      const variant = url.searchParams.get("image");
      const key = original.get(digest);
      const name = `${key ?? "legacy"}:${variant ?? "plain"}`;
      reads[name] = (reads[name] ?? 0) + 1;
      if (key === undefined) {
        if (variant !== null || extension !== "webp") { response.statusCode = 404; response.end(); return; }
        response.setHeader("content-type", "image/webp"); response.end(fixture(`optimized/${digest}.webp`)); return;
      }
      if (variant === null) {
        response.setHeader("content-type", ORIGINALS[key as keyof typeof ORIGINALS].media_type);
        response.setHeader("x-content-type-options", "nosniff"); response.end(fixture("landscape.png")); return;
      }
      const rendition = RENDITION[variant];
      if (url.searchParams.get("v") !== "1" || rendition === undefined) { response.statusCode = 400; response.end(); return; }
      const bytes = fixture(`optimized/${rendition}.webp`);
      response.setHeader("content-type", "image/webp");
      // The slow original's tile waits until the test releases it: a first
      // view whose rendition the daemon is still encoding.
      if (key === "slow" && (variant === "tile" || variant === "tile2x")) held.push(() => response.end(bytes));
      else response.end(bytes);
    });
  } }],
  server: { hmr: false, watch: null, host: "127.0.0.1", port: 5179,
    headers: { "x-rosetta-protocol-version": String(PROTOCOL_VERSION) } },
});
