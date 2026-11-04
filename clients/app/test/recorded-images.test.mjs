// Deferred image renditions (2026-11-01): a transcript image is a recorded
// original (its own bytes as one member, no rendition map) or, in a projection
// built earlier, a complete legacy WebP bundle — and one transcript may hold
// both. Either is DRAWN only through WebP the daemon encoded: every rendition
// the room asks of rosetta's host is a `?image=<variant>&v=1` address for an
// original, 1:1 included; the plain member URL is for opening or saving.
// Fixtures are synthesized here; no daemon, archive or home directory.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { RosettaClient } from "../../ts/client.ts";
import { assetsOver } from "../renderer/src/host/rosetta-ports.ts";
import { createAttachmentReader } from "../../room/src/components/transcript/attachment-file-source.ts";
import { attachmentGallery, createAttachmentGallerySource } from "../../room/src/components/transcript/attachment-gallery.ts";
import { handleForTimelineMetaArtifact, timelineMetaFileSource } from "../../room/src/components/transcript/timeline-meta-file-source.ts";
import { Attachments } from "../../room/src/components/transcript/Attachments.tsx";
import { resolveKind } from "../../room/src/components/file-viewer/registry.ts";
import { RoomProvider } from "../../room/src/room-context.tsx";
import { createDemoPorts } from "../../room/src/fixtures/index.ts";
import { timelineImages } from "../../room/src/lib/transcript/images.ts";
import { entry, inline, read } from "./support/ledger-fixtures.mjs";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const { createElement } = fromRoom("react");
const { renderToStaticMarkup } = fromRoom("react-dom/server");

const BASE = "http://127.0.0.1:4640";
const RENDITIONS = ["rail", "tile", "tile2x", "preview", "full"];
const FORMATS = [["image/png", "png"], ["image/jpeg", "jpg"], ["image/gif", "gif"], ["image/webp", "webp"], ["image/bmp", "bmp"], ["image/x-icon", "ico"]];
const hex = (digit) => digit.repeat(64);
const leaf = (digit) => ({ url: `/api/assets/${hex(digit)}.webp`, digest: hex(digit), media_type: "image/webp", byte_len: 42 });
const bundle = () => ({ ...leaf("a"), image_variants: { rail: leaf("b"), tile: leaf("c"), tile2x: leaf("d"), preview: leaf("e") } });
const original = (media_type = "image/png", extension = "png", digit = "1") =>
  ({ url: `/api/assets/${hex(digit)}.${extension}`, digest: hex(digit), media_type, byte_len: 4096 });
const attachment = (id, asset, over = {}) => ({ id, kind: "image", label: `[Image #${id}]`, name: `capture-${id}.png`,
  media_type: asset.media_type, byte_len: asset.byte_len, dimensions: { width: 1440, height: 900 }, message_ranges: [],
  source: { type: "asset", asset }, ...over });
const refusedRead = async () => { throw new Error("a recorded image is drawn by its renditions, never read"); };

/** A legacy bundle beside originals in each format, then a raw member no
 * gate admits, across a prompt and a tool read. */
function mixedTranscript() {
  const prompt = entry({ type: "user", text: inline("Compare [Image #1] with [Image #2]."), attachments: [
    attachment("1", bundle()), attachment("2", original()),
  ] }, { timeline_id: "mixed-prompt" });
  const tool = read("/gone/screens.png", { timeline_id: "mixed-read", attachments: [
    ...FORMATS.map(([mediaType, extension], index) => attachment(String(index + 3), original(mediaType, extension, String(index + 2)))),
    attachment("raw", { url: "/test/fixtures/prompt-images/landscape.png", digest: hex("f"), media_type: "image/png", byte_len: 4479 }),
  ] });
  return timelineImages([prompt, tool]);
}

test("through rosetta's host, every rendition of a recorded original is a daemon WebP, 1:1 included", () => {
  const assets = assetsOver(new RosettaClient({ baseUrl: BASE }));
  const reader = createAttachmentReader(assets, refusedRead);
  const facts = mixedTranscript();
  assert.equal(facts.length, 9);
  const [legacy, png, ...rest] = facts;
  const raw = rest.pop();
  assert.equal(reader.imageResource(raw), null, "a raw member no ingestion wrote is not drawn");
  assert.equal(reader.canOpen(raw), false);
  for (const fact of [png, ...rest]) {
    const asset = fact.ref.source.asset;
    const resource = reader.imageResource(fact);
    assert.ok(resource, asset.media_type);
    assert.equal(reader.canOpen(fact), true);
    for (const variant of RENDITIONS) {
      const url = new URL(resource.url(variant));
      assert.equal(`${url.origin}${url.pathname}`, `${BASE}${asset.url}`);
      assert.deepEqual([...url.searchParams], [["image", variant], ["v", "1"]], `${asset.media_type} ${variant}`);
    }
    assert.equal(reader.assetUrl(fact), `${BASE}${asset.url}`, "the plain address stays the identity, for opening or saving");
  }
  const bundled = legacy.ref.source.asset;
  assert.deepEqual(RENDITIONS.map((variant) => reader.imageResource(legacy).url(variant)),
    [...["rail", "tile", "tile2x", "preview"].map((variant) => `${BASE}${bundled.image_variants[variant].url}`), `${BASE}${bundled.url}`],
    "a legacy bundle keeps exactly its own members");
});

test("the gallery of a mixed row opens both forms by their renditions and reads no bytes", () => {
  const asked = [];
  const assets = { imageAssetUrl: (asset, variant) => { asked.push([asset.digest, variant]); return `${BASE}/r/${asset.digest}/${variant}`; },
    attachmentAssetUrl: (asset) => `${BASE}${asset.url}` };
  const reader = createAttachmentReader(assets, refusedRead);
  const facts = mixedTranscript();
  const { images, initialKey } = attachmentGallery(reader, facts[1], facts);
  assert.equal(initialKey, facts[1].key);
  assert.deepEqual(images.map((image) => image.handle !== null), [true, true, true, true, true, true, true, true, false]);
  const source = createAttachmentGallerySource(reader);
  for (const image of images.filter((item) => item.handle !== null)) {
    assert.equal(resolveKind(image.handle.name).viewer.id, "image", image.handle.name);
    const resource = source.image(image.handle);
    assert.equal(resource.url("full"), `${BASE}/r/${image.handle.ref.ref.source.asset.digest}/full`);
  }
  assert.equal(asked.length, 8);
  assert.ok(asked.every(([, variant]) => variant === "full"));
});

test("an asset-backed artifact that is a recorded image opens by its renditions and reads nothing", async () => {
  const asked = [];
  const assets = { imageAssetUrl: (asset, variant) => { asked.push(variant); return `${BASE}${asset.url}?image=${variant}&v=1`; },
    attachmentAssetUrl: () => null };
  const sessions = { run: () => { throw new Error("nothing is read"); } };
  for (const [mediaType, extension] of FORMATS) {
    const asset = original(mediaType, extension);
    const handle = await handleForTimelineMetaArtifact(sessions, assets, "s1", {
      id: "art", name: "Revised view", artifact_kind: "image", created_at: null, byte_len: asset.byte_len,
      media_type: mediaType, source: { type: "asset", asset },
    });
    assert.equal(handle.name, `Revised view.${extension}`);
    assert.equal(resolveKind(handle.name).viewer.id, "image", handle.name);
    assert.equal(timelineMetaFileSource.image(handle).url("full"), `${BASE}${asset.url}?image=full&v=1`);
    await assert.rejects(timelineMetaFileSource.read(handle, new AbortController().signal), /no longer available/);
  }
  assert.deepEqual(asked, Array(FORMATS.length).fill("full"));
  // A name that already says "image" is kept; any other gains the format's.
  const kept = await handleForTimelineMetaArtifact(sessions, assets, "s1", { id: "a2", name: "shot.png", artifact_kind: "image",
    created_at: null, byte_len: 1, media_type: "image/jpeg", source: { type: "asset", asset: original("image/jpeg", "jpg") } });
  assert.equal(kept.name, "shot.png");
  const svgNamed = await handleForTimelineMetaArtifact(sessions, assets, "s1", { id: "a3", name: "shot.svg", artifact_kind: "image",
    created_at: null, byte_len: 1, media_type: "image/png", source: { type: "asset", asset: original() } });
  assert.equal(svgNamed.name, "shot.svg.png");
  await assert.rejects(handleForTimelineMetaArtifact(sessions, assets, "s1", { id: "a4", name: "raw", artifact_kind: "image",
    created_at: null, byte_len: 1, media_type: "image/png",
    source: { type: "asset", asset: { ...original(), url: "/test/raw.png" } } }), /not a recorded member/);
});

test("an asset-backed artifact that is not an image still reads its bytes", async () => {
  const bytes = new TextEncoder().encode("# Plan");
  const reads = [];
  const sessions = { run: (_id, read) => read({ readAsset: async (asset) => { reads.push(asset.url); return bytes; } }) };
  const asset = { url: `/api/assets/${hex("7")}.txt`, digest: hex("7"), media_type: "text/plain; charset=utf-8", byte_len: bytes.byteLength };
  const handle = await handleForTimelineMetaArtifact(sessions, { imageAssetUrl: () => null, attachmentAssetUrl: () => null }, "s1", {
    id: "plan", name: "notes", artifact_kind: "document", created_at: null, byte_len: bytes.byteLength,
    media_type: asset.media_type, source: { type: "asset", asset },
  });
  assert.deepEqual(reads, [asset.url]);
  assert.equal(handle.name, "notes");
  assert.equal(timelineMetaFileSource.image(handle), null);
  assert.deepEqual(await timelineMetaFileSource.read(handle, new AbortController().signal), bytes);
});

function renderTiles(imageAssetUrl, facts = mixedTranscript().slice(0, 2)) {
  const ports = { ...createDemoPorts(), assets: { imageAssetUrl, attachmentAssetUrl: (asset) => `${BASE}${asset.url}` } };
  // A tile picks its density from the window; this file's process has none.
  globalThis.window ??= { devicePixelRatio: 1 };
  return renderToStaticMarkup(createElement(RoomProvider, { ports },
    createElement(Attachments, { facts, onOpenAttachment: () => {} })));
}
const tiles = (markup) => [...markup.matchAll(/<(button|div) [^>]*class="(tr-att__tile[^"]*)"[^>]*>/g)].map((match) => ({
  className: match[2], busy: /aria-busy="([^"]*)"/.exec(match[0])?.[1] ?? null,
}));

test("a tile whose rendition is still being fetched is busy in the loading style", () => {
  const markup = renderTiles(() => new Promise(() => {}));
  assert.deepEqual(tiles(markup), [
    { className: "tr-att__tile tr-att__tile--loading", busy: "true" },
    { className: "tr-att__tile tr-att__tile--loading", busy: "true" },
  ]);
  assert.doesNotMatch(markup, /<img/);
});

test("an offscreen tile asks for nothing and is not busy; a refused one says it could not load", () => {
  // Server markup has no intersection observer: every tile is offscreen.
  assert.deepEqual(tiles(renderTiles((asset, variant) => `${BASE}${asset.url}?image=${variant}&v=1`)), [
    { className: "tr-att__tile", busy: "false" },
    { className: "tr-att__tile", busy: "false" },
  ]);
  const refused = renderTiles(() => null);
  assert.deepEqual(tiles(refused).map((tile) => tile.className), ["tr-att__tile tr-att__tile--missing", "tr-att__tile tr-att__tile--missing"]);
  assert.match(refused, /Could not load image/);
});

test("a mixed strip draws both recorded forms as openable tiles and the raw member as missing", () => {
  const markup = renderTiles((asset, variant) => `${BASE}${asset.url}?image=${variant}&v=1`, mixedTranscript());
  const drawn = tiles(markup);
  assert.equal(drawn.length, 9);
  assert.deepEqual(drawn.map((tile) => tile.className.includes("--missing")), [false, false, false, false, false, false, false, false, true]);
  assert.match(markup, /Image data was not recorded/);
});
