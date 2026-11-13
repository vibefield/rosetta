// Sent videos in the attachment rail (2026-11-12). A tool row's attachment of
// kind `video` is a recorded member (`/api/assets/<digest>.mp4` or `.mov`) or
// a reference to a file that was sent but not captured. The rail gathers it
// beside the run's images, in order, and plays it in place: nothing is asked
// before the reader presses play, and a video never enters an image path.
// Everything here is synthesized; no daemon, archive or home directory.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { RosettaClient } from "../../ts/client.ts";
import { assetsOver } from "../renderer/src/host/rosetta-ports.ts";
import { Attachments, attachmentStatus } from "../../room/src/components/transcript/Attachments.tsx";
import { VideoFace, videoAspect, videoStatus } from "../../room/src/components/transcript/VideoTile.tsx";
import { createAttachmentReader } from "../../room/src/components/transcript/attachment-file-source.ts";
import { attachmentGallery, createAttachmentGallerySource } from "../../room/src/components/transcript/attachment-gallery.ts";
import { IDLE_VIDEO, createVideoSource } from "../../room/src/components/transcript/video-source.ts";
import { createDemoPorts, createRoomDemo } from "../../room/src/fixtures/index.ts";
import { fmtBytes } from "../../room/src/lib/format.ts";
import { isRecordedImageAsset, isRecordedVideoAsset } from "../../room/src/lib/image-media.ts";
import { activityImages, isRailMedia, messageAttachments, timelineImages } from "../../room/src/lib/transcript/images.ts";
import { ledgerLine } from "../../room/src/lib/transcript/index.ts";
import { RoomProvider } from "../../room/src/room-context.tsx";
import { entry, file, inline, tool } from "./support/ledger-fixtures.mjs";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const { createElement } = fromRoom("react");
const { renderToStaticMarkup } = fromRoom("react-dom/server");

const BASE = "http://127.0.0.1:4640";
const hex = (digit) => digit.repeat(64);
const png = (digit = "1") => ({ url: `/api/assets/${hex(digit)}.png`, digest: hex(digit), media_type: "image/png", byte_len: 4096 });
const mp4 = (digit = "a") => ({ url: `/api/assets/${hex(digit)}.mp4`, digest: hex(digit), media_type: "video/mp4", byte_len: 3_562_144 });
const mov = (digit = "b") => ({ url: `/api/assets/${hex(digit)}.mov`, digest: hex(digit), media_type: "video/quicktime", byte_len: 11_482_112 });
const leaf = (digit) => ({ url: `/api/assets/${hex(digit)}.webp`, digest: hex(digit), media_type: "image/webp", byte_len: 42 });
const bundle = () => ({ ...leaf("c"), image_variants: { rail: leaf("d"), tile: leaf("e"), tile2x: leaf("f"), preview: leaf("0") } });
const reference = (pathname) => ({ type: "reference", reference: { type: "file", path: pathname, resolved: true } });

const image = (id, asset = png(), over = {}) => ({ id, kind: "image", label: `[Image #${id}]`, name: `shot-${id}.png`,
  media_type: asset.media_type, byte_len: asset.byte_len, dimensions: { width: 1440, height: 900 }, message_ranges: [],
  source: { type: "asset", asset }, ...over });
const video = (id, asset = mp4(), over = {}) => ({ id, kind: "video", label: null, name: `clip-${id}.mp4`,
  media_type: asset.media_type, byte_len: asset.byte_len, dimensions: { width: 1920, height: 1080 }, message_ranges: [],
  source: { type: "asset", asset }, ...over });
const uncaptured = (id, kind = "video") => ({ id, kind, label: null, name: kind === "video" ? "before.mov" : "before.png",
  media_type: kind === "video" ? "video/quicktime" : "image/png", byte_len: 11_482_112, dimensions: null, message_ranges: [],
  source: reference(kind === "video" ? "/w/before.mov" : "/w/before.png") });

const send = (attachments, over = {}) => tool({ type: "artifact", operation: "send", artifact_kind: "file", name: null,
  description: inline("What changed"), resources: [file("/w/clip.mp4"), file("/w/before.mov")], media: null }, { attachments, ...over });

/** A prompt with an image, then a send of a video, an image, a video that was
 * not captured and a document, then a reply's image. */
function mixedRun() {
  const prompt = entry({ type: "user", text: inline("Look at [Image #1]."), attachments: [image("1")] }, { timeline_id: "prompt" });
  const sent = send([video("1"), image("2", png("2")), uncaptured("3"),
    { ...video("4"), kind: "document", name: "notes.pdf", media_type: "application/pdf" }], { timeline_id: "sent" });
  const reply = entry({ type: "agent", text: inline("Done."), attachments: [video("1", mov()), image("2", png("3"))], citations: [] },
    { timeline_id: "reply" });
  return { prompt, sent, reply };
}

const recordingAssets = (over = {}) => {
  const asked = { image: [], attachment: [], media: [] };
  return { asked, assets: {
    imageAssetUrl: (asset, variant) => { asked.image.push([asset.media_type, variant]); return `${BASE}${asset.url}?image=${variant}&v=1`; },
    attachmentAssetUrl: (asset) => { asked.attachment.push(asset.url); return `${BASE}${asset.url}`; },
    mediaAssetUrl: (asset) => { asked.media.push(asset.url); return `${BASE}${asset.url}`; },
    ...over,
  } };
};
const refusedRead = async () => { throw new Error("nothing is read before the reader asks"); };

/* ── the gather ─────────────────────────────────────────────────────────── */

test("the rail gathers videos beside images, in transcript order, and nothing else", () => {
  const { prompt, sent, reply } = mixedRun();
  const facts = timelineImages([prompt, sent, reply, sent]);
  assert.deepEqual(facts.map((fact) => [fact.kind, fact.key]), [
    ["image", "s1:prompt:attachment:1"],
    ["video", "s1:sent:attachment:1"], ["image", "s1:sent:attachment:2"], ["video", "s1:sent:attachment:3"],
    ["video", "s1:reply:attachment:1"], ["image", "s1:reply:attachment:2"],
  ], "order is the transcript's, a repeated row is one occurrence, a document is not gathered");
  assert.equal(facts[3].missing, true, "a video that was not captured keeps its slot");
  assert.deepEqual([facts[3].name, facts[3].mediaType, facts[3].byteLen], ["before.mov", "video/quicktime", 11_482_112]);
  assert.deepEqual(activityImages([{ kind: "line", line: ledgerLine(sent) }]).map((fact) => fact.kind), ["video", "image", "video"]);
  assert.deepEqual(messageAttachments(sent).map(isRailMedia), [true, true, true, false]);
});

test("a gathered video reaches no image path: no rendition, no viewer handle, no gallery slot", async () => {
  const { prompt, sent, reply } = mixedRun();
  const facts = timelineImages([prompt, sent, reply]);
  const { asked, assets } = recordingAssets();
  const reader = createAttachmentReader(assets, refusedRead);
  const videos = facts.filter((fact) => fact.kind === "video");
  const images = facts.filter((fact) => fact.kind === "image");
  assert.equal(videos.length, 3);
  for (const fact of videos) {
    assert.equal(reader.imageResource(fact), null, fact.key);
    assert.equal(reader.canOpen(fact), false, fact.key);
  }
  for (const fact of images) {
    assert.equal(reader.canOpen(fact), true, fact.key);
    assert.equal(reader.videoAsset(fact), null, "an image is never a video");
    reader.imageResource(fact).url("tile");
  }
  // The gallery of the whole row holds the images only, each still openable.
  const { images: slots, initialKey } = attachmentGallery(reader, images[1], facts);
  assert.equal(initialKey, images[1].key);
  assert.deepEqual(slots.map((slot) => slot.key), images.map((fact) => fact.key));
  assert.ok(slots.every((slot) => slot.handle !== null));
  const source = createAttachmentGallerySource(reader);
  for (const slot of slots) source.image(slot.handle).url("full");
  // A video handed to the gallery's reader anyway is refused, not read.
  await assert.rejects(source.read({ key: "v", name: "clip.mp4", path: null, size: 1, ref: videos[0] }, new AbortController().signal),
    /Image data was not recorded/);
  assert.equal(source.image({ key: "v", name: "clip.mp4", path: null, size: 1, ref: videos[0] }), null);
  assert.ok(asked.image.length > 0);
  assert.ok(asked.image.every(([mediaType]) => mediaType.startsWith("image/")), "imageAssetUrl is asked for images only");
  assert.deepEqual(asked.media, [], "and no video address is asked by gathering or opening");
});

/* ── the member ─────────────────────────────────────────────────────────── */

test("a recorded video is an mp4 or mov member with no rendition map; nothing else is", () => {
  assert.equal(isRecordedVideoAsset(mp4()), true);
  assert.equal(isRecordedVideoAsset(mov()), true);
  for (const [why, asset] of [
    ["a recorded image", png()],
    ["a legacy bundle", bundle()],
    ["a video with a rendition map", { ...mp4(), image_variants: {} }],
    ["a bundle wearing a video address", { ...bundle(), url: mp4().url, digest: mp4().digest, media_type: "video/mp4" }],
    ["another route", { ...mp4(), url: `/api/files/${hex("a")}.mp4` }],
    ["another origin", { ...mp4(), url: `https://example.test/api/assets/${hex("a")}.mp4` }],
    ["a query", { ...mp4(), url: `${mp4().url}?image=tile&v=1` }],
    ["a short digest", { ...mp4(), url: "/api/assets/abc.mp4", digest: "abc" }],
    ["an upper-case digest", { ...mp4(), url: `/api/assets/${hex("A")}.mp4`, digest: hex("A") }],
    ["a name that is not its digest", { ...mp4(), url: `/api/assets/${hex("9")}.mp4` }],
    ["another container", { ...mp4(), url: `/api/assets/${hex("a")}.webm`, media_type: "video/webm" }],
    ["an extension that is not its type's", { ...mp4(), url: `/api/assets/${hex("a")}.mov` }],
    ["a type that is not its extension's", { ...mov(), media_type: "video/mp4" }],
    ["no bytes", { ...mp4(), byte_len: 0 }],
    ["a missing asset", undefined],
    ["a null asset", null],
  ]) assert.equal(isRecordedVideoAsset(asset), false, why);
  assert.equal(isRecordedImageAsset(mp4()), false, "and a video member is never a recorded image");
  assert.equal(isRecordedImageAsset(mov()), false);
});

test("the reader offers a captured video's member and nothing for the rest", () => {
  const { assets } = recordingAssets();
  const reader = createAttachmentReader(assets, refusedRead);
  const [captured, quick, sent, raw, wrongKind] = messageAttachments(send([
    video("1"), video("2", mov()), uncaptured("3"),
    video("4", { ...mp4("9"), url: "/test/fixtures/clip.mp4" }), image("5", mp4("8")),
  ]));
  assert.equal(reader.videoAsset(captured), captured.ref.source.asset);
  assert.equal(reader.videoAsset(quick), quick.ref.source.asset);
  assert.equal(reader.videoAsset(sent), null, "sent, not captured");
  assert.equal(reader.videoAsset(raw), null, "a member no ingestion wrote");
  assert.equal(reader.videoAsset(wrongKind), null, "an image slot holding a video member is neither");
  assert.equal(reader.imageResource(wrongKind), null);
  assert.equal(reader.assetUrl(captured), `${BASE}${mp4().url}`, "the plain address stays the identity");
  assert.equal(reader.assetUrl(raw), null);
  assert.equal(typeof reader.mediaAssetUrl, "function");
  assert.equal(reader.mediaAssetUrl(mp4()), `${BASE}${mp4().url}`);
  // A host that does not offer the member offers no video either.
  const closed = createAttachmentReader({ imageAssetUrl: () => null, attachmentAssetUrl: () => null }, refusedRead);
  assert.equal(closed.videoAsset(captured), null);
  assert.equal(closed.mediaAssetUrl, undefined, "a host without the door has none");
});

test("rosetta's host gives a video no address: the daemon serves it whole, which cannot seek", () => {
  const assets = assetsOver(new RosettaClient({ baseUrl: BASE }));
  assert.equal(assets.mediaAssetUrl, undefined, "the room reads the bytes and plays an object URL instead");
  assert.equal(assets.attachmentAssetUrl(mp4()), `${BASE}/api/assets/${hex("a")}.mp4`, "the member's identity stays");
  assert.equal(assets.attachmentAssetUrl(mov()), `${BASE}/api/assets/${hex("b")}.mov`);
  assert.equal(assets.attachmentAssetUrl({ ...mp4(), url: "https://example.test/clip.mp4" }), null, "only that daemon");
});

/* ── the load ───────────────────────────────────────────────────────────── */

/** Fake doors, recording every ask. `read` settles when the test says. */
function doors(over = {}) {
  const log = { media: [], reads: [], made: [], revoked: [], states: [] };
  let settle = null;
  const fake = {
    log,
    resolveRead: (bytes) => settle.resolve(bytes),
    rejectRead: (error) => settle.reject(error),
    doors: {
      readAsset: (asset, signal) => {
        log.reads.push({ url: asset.url, signal });
        return new Promise((resolve, reject) => { settle = { resolve, reject }; });
      },
      createObjectURL: (bytes, mediaType) => {
        const url = `blob:fake/${log.made.length + 1}`;
        log.made.push({ url, bytes, mediaType });
        return url;
      },
      revokeObjectURL: (url) => { log.revoked.push(url); },
      onChange: (state) => { log.states.push(state.phase === "ready" ? `ready:${state.from}` : state.phase); },
      ...over,
    },
  };
  return fake;
}
const hostUrl = (log) => (asset) => { log.media.push(asset.url); return `${BASE}${asset.url}`; };
const tick = () => new Promise((resolve) => setImmediate(resolve));
const BYTES = new Uint8Array([0, 0, 0, 24, 102, 116, 121, 112]);

test("a video source asks nothing until the reader asks to play", async () => {
  const fake = doors();
  fake.doors.mediaAssetUrl = hostUrl(fake.log);
  const source = createVideoSource(mp4(), fake.doors);
  assert.equal(source.state(), IDLE_VIDEO);
  source.elementFailed(`${BASE}${mp4().url}`);
  await tick();
  assert.deepEqual(fake.log, { media: [], reads: [], made: [], revoked: [], states: [] });
  assert.deepEqual(source.state(), { phase: "idle" });
});

test("with a host address, play is that address and no byte is read", async () => {
  const fake = doors();
  fake.doors.mediaAssetUrl = hostUrl(fake.log);
  const source = createVideoSource(mp4(), fake.doors);
  source.play();
  assert.deepEqual(source.state(), { phase: "ready", url: `${BASE}${mp4().url}`, from: "host" });
  source.play();
  await tick();
  assert.deepEqual(fake.log.media, [mp4().url], "a second press while ready asks nothing");
  assert.deepEqual(fake.log.states, ["ready:host"]);
  assert.deepEqual([fake.log.reads, fake.log.made, fake.log.revoked], [[], [], []]);
  source.dispose();
  assert.deepEqual(fake.log.revoked, [], "the host's address is the host's to revoke");
});

test("a host address that arrives later is awaited as loading", async () => {
  const fake = doors();
  let answer;
  fake.doors.mediaAssetUrl = () => new Promise((resolve) => { answer = resolve; });
  const source = createVideoSource(mov(), fake.doors);
  source.play();
  assert.deepEqual(source.state(), { phase: "loading" });
  answer(`blob:host/1`);
  await tick();
  assert.deepEqual(source.state(), { phase: "ready", url: "blob:host/1", from: "host" });
  assert.deepEqual(fake.log.states, ["loading", "ready:host"]);
  assert.deepEqual(fake.log.reads, []);
});

test("without a host address, play reads the bytes and plays an object URL", async () => {
  const fake = doors();
  const source = createVideoSource(mov(), fake.doors);
  source.play();
  assert.deepEqual(source.state(), { phase: "loading" });
  assert.equal(fake.log.reads.length, 1);
  assert.equal(fake.log.reads[0].url, mov().url);
  assert.equal(fake.log.reads[0].signal.aborted, false);
  source.play();
  assert.equal(fake.log.reads.length, 1, "a press while loading is nothing");
  fake.resolveRead(BYTES);
  await tick();
  assert.deepEqual(source.state(), { phase: "ready", url: "blob:fake/1", from: "bytes" });
  assert.deepEqual(fake.log.made, [{ url: "blob:fake/1", bytes: BYTES, mediaType: "video/quicktime" }]);
  assert.deepEqual(fake.log.states, ["loading", "ready:bytes"]);
  assert.deepEqual(fake.log.revoked, []);
});

test("a host that answers null, nothing, a refusal or a throw falls to the bytes", async () => {
  for (const [why, answer] of [
    ["null", () => null], ["an empty address", () => ""], ["undefined", () => undefined],
    ["a promise of null", () => Promise.resolve(null)], ["a rejection", () => Promise.reject(new Error("no"))],
    ["a throw", () => { throw new Error("no"); }],
  ]) {
    const fake = doors({ mediaAssetUrl: answer });
    const source = createVideoSource(mp4(), fake.doors);
    source.play();
    await tick();
    assert.equal(fake.log.reads.length, 1, why);
    fake.resolveRead(BYTES);
    await tick();
    assert.deepEqual(source.state(), { phase: "ready", url: "blob:fake/1", from: "bytes" }, why);
    assert.deepEqual(fake.log.states, ["loading", "ready:bytes"], why);
  }
});

test("when the element cannot load the host's address, the bytes are read; when it cannot play those, it failed", async () => {
  const fake = doors();
  fake.doors.mediaAssetUrl = hostUrl(fake.log);
  const source = createVideoSource(mp4(), fake.doors);
  source.play();
  const address = source.state().url;
  source.elementFailed("blob:some/other");
  assert.equal(source.state().phase, "ready", "an error about another address is nothing");
  source.elementFailed(address);
  assert.deepEqual(source.state(), { phase: "loading" });
  assert.equal(fake.log.reads.length, 1);
  source.elementFailed(address);
  assert.equal(fake.log.reads.length, 1, "a late repeat of the same error reads nothing more");
  fake.resolveRead(BYTES);
  await tick();
  assert.deepEqual(source.state(), { phase: "ready", url: "blob:fake/1", from: "bytes" });
  assert.deepEqual(fake.log.made.map((made) => made.mediaType), ["video/mp4"]);
  source.elementFailed("blob:fake/1");
  assert.deepEqual(source.state(), { phase: "failed" });
  assert.deepEqual(fake.log.revoked, ["blob:fake/1"], "the object URL that could not play is revoked");
  assert.deepEqual(fake.log.states, ["ready:host", "loading", "ready:bytes", "failed"]);
  // Retry asks the host again: a host must not keep a failure, nor does the room.
  source.play();
  assert.deepEqual(fake.log.media, [mp4().url, mp4().url]);
  assert.equal(source.state().phase, "ready");
  assert.equal(source.state().from, "host");
});

test("a read that rejects is the failed state, and Retry reads again", async () => {
  const fake = doors();
  const source = createVideoSource(mp4(), fake.doors);
  source.play();
  fake.rejectRead(new Error("the daemon is away"));
  await tick();
  assert.deepEqual(source.state(), { phase: "failed" });
  assert.deepEqual(fake.log.states, ["loading", "failed"]);
  assert.deepEqual([fake.log.made, fake.log.revoked], [[], []]);
  source.play();
  assert.equal(fake.log.reads.length, 2);
  fake.resolveRead(BYTES);
  await tick();
  assert.equal(source.state().phase, "ready");
  // A read door that throws instead of rejecting, and a URL that cannot be made, fail the same way.
  const thrown = doors({ readAsset: () => { throw new Error("no session"); } });
  const second = createVideoSource(mp4(), thrown.doors);
  second.play();
  await tick();
  assert.deepEqual(second.state(), { phase: "failed" });
  const unmade = doors({ createObjectURL: () => { throw new Error("no URL"); } });
  const third = createVideoSource(mp4(), unmade.doors);
  third.play();
  unmade.resolveRead(BYTES);
  await tick();
  assert.deepEqual(third.state(), { phase: "failed" });
});

test("dispose aborts a read in flight, and its late bytes make no URL", async () => {
  const fake = doors();
  const source = createVideoSource(mp4(), fake.doors);
  source.play();
  const { signal } = fake.log.reads[0];
  source.dispose();
  assert.equal(signal.aborted, true);
  fake.resolveRead(BYTES);
  await tick();
  assert.deepEqual(fake.log.made, [], "nothing is made for a tile that left");
  assert.deepEqual(fake.log.states, ["loading"], "and nothing is announced after disposal");
  source.play();
  source.elementFailed("blob:fake/1");
  assert.equal(fake.log.reads.length, 1, "a disposed source asks nothing more");
});

test("dispose revokes the object URL it made, once", async () => {
  const fake = doors();
  const source = createVideoSource(mp4(), fake.doors);
  source.play();
  fake.resolveRead(BYTES);
  await tick();
  assert.equal(source.state().url, "blob:fake/1");
  source.dispose();
  source.dispose();
  assert.deepEqual(fake.log.revoked, ["blob:fake/1"]);
});

/* ── the tile ───────────────────────────────────────────────────────────── */

function renderRail(facts, assets = recordingAssets().assets) {
  const reads = [];
  const demo = createDemoPorts();
  const client = demo.sessions.open("s0");
  const ports = { ...demo, assets,
    sessions: { open: () => ({ ...client, readAsset: async (asset) => { reads.push(asset.url); return BYTES; } }), close() {} } };
  // An image tile picks its density from the window; this process has none.
  globalThis.window ??= { devicePixelRatio: 1 };
  const markup = renderToStaticMarkup(createElement(RoomProvider, { ports },
    createElement(Attachments, { facts, onOpenAttachment: () => { throw new Error("a video opens nothing"); } })));
  return { markup, reads };
}
const figures = (markup) => [...markup.matchAll(/<figure\b.*?<\/figure>/gs)].map((match) => match[0]);
const face = (props) => renderToStaticMarkup(createElement(VideoFace, { index: 0, playable: true, state: IDLE_VIDEO, ...props }));

test("before play, a captured video is a button with a glyph, its name and its size, and asks nothing", () => {
  const { asked, assets } = recordingAssets();
  const [fact] = messageAttachments(send([video("1")]));
  const { markup, reads } = renderRail([fact], assets);
  const [figure] = figures(markup);
  assert.match(figure, /^<figure class="tr-att__video" data-attachment-id="[^"]+" data-video="idle"/);
  assert.match(figure, /style="--tr-video-aspect:1\.7778"/, "the video's own shape at the rail's height");
  assert.ok(figure.includes(`<button type="button" aria-busy="false" class="tr-att__tile tr-att__tile--video" aria-label="Play video — clip-1.mp4 · ${fmtBytes(3_562_144)}">`), figure);
  assert.match(figure, /<svg [^>]*aria-hidden="true">/);
  assert.match(figure, /<span class="tr-att__name">clip-1\.mp4<\/span>/);
  assert.match(figure, new RegExp(`<span class="tr-att__facts">${fmtBytes(3_562_144)}</span>`));
  assert.doesNotMatch(markup, /<video|<img|poster|Retry/);
  assert.deepEqual([asked.image, asked.media, reads], [[], [], []], "no poster, no thumbnail, no address, no bytes");
  assert.deepEqual(asked.attachment.length > 0 && asked.attachment.every((url) => url === mp4().url), true, "only its identity");
});

test("a video that was not captured keeps the tile's shape and says so; it is not a button and has no Retry", () => {
  const [fact] = messageAttachments(send([uncaptured("1")]));
  const { markup } = renderRail([fact]);
  const [figure] = figures(markup);
  assert.match(figure, /^<figure class="tr-att__video" data-attachment-id="[^"]+" data-video="uncaptured"/);
  assert.match(figure, /style="--tr-video-aspect:1\.7778"/, "16:9 when the dimensions are unknown");
  assert.match(figure, /<div class="tr-att__tile tr-att__tile--video tr-att__tile--missing" tabindex="-1"/);
  assert.match(figure, /<span>Video not captured<\/span>/);
  assert.match(figure, new RegExp(`<span class="tr-att__facts">before\\.mov · ${fmtBytes(11_482_112)}</span>`));
  assert.doesNotMatch(figure, /<button|<video|Retry|Play video/);
});

test("a failed load is the could-not-play face with Retry; a loading one is busy; a ready one is the video", () => {
  const [fact] = messageAttachments(send([video("1")]));
  const failed = face({ fact, state: { phase: "failed" } });
  assert.match(failed, /data-video="failed"/);
  assert.match(failed, /<div class="tr-att__tile tr-att__tile--video tr-att__tile--missing" tabindex="-1" aria-label="Could not play video — /);
  assert.match(failed, /<span>Could not play video<\/span><button type="button" class="tr-att__retry" aria-label="Retry video — clip-1\.mp4">Retry<\/button>/,
    "Retry names what it retries: focus lands on it when a load fails");
  assert.doesNotMatch(failed, /<video|Play video/);
  const loading = face({ fact, state: { phase: "loading" } });
  assert.match(loading, /data-video="loading"/);
  assert.match(loading, /<button type="button" aria-busy="true" class="tr-att__tile tr-att__tile--video tr-att__tile--loading" aria-label="Loading video — clip-1\.mp4">/,
    "the name a screen reader hears says it is loading, as the visible words do");
  assert.match(loading, /<span class="tr-att__facts">Loading video…<\/span>/);
  assert.doesNotMatch(loading, /<video|Play video/);
  const url = `${BASE}${mp4().url}`;
  const ready = face({ fact, state: { phase: "ready", url, from: "host" } });
  assert.match(ready, /data-video="ready"/);
  assert.match(ready, new RegExp(`<div class="tr-att__tile tr-att__tile--video tr-att__tile--playing" tabindex="-1"><video src="${url.replaceAll(".", "\\.")}" controls="" autoPlay="" playsInline="" preload="auto" aria-label="clip-1\\.mp4"></video></div>`));
  assert.doesNotMatch(ready, /poster|<button|<img/);
  // A video that cannot be played here shows why, whatever state it is handed.
  const closed = face({ fact, playable: false, state: { phase: "ready", url, from: "host" } });
  assert.match(closed, /data-video="uncaptured"/);
  assert.match(closed, /<span>Video is not available here<\/span>/);
  assert.doesNotMatch(closed, /<video|<button|not recorded/);
});

test("a recorded video the room cannot address says it is not available here, never that it was not recorded", () => {
  const [captured, foreign, sent, omitted, unrecorded, bare] = messageAttachments(send([
    video("1"), video("2", { ...mp4("9"), url: "/test/fixtures/clip.mp4" }), uncaptured("3"),
    video("4", mp4(), { source: { type: "unavailable", reason: "omitted" } }),
    video("5", mp4(), { source: { type: "unavailable", reason: "not_recorded" } }),
  ])).concat([{ ...messageAttachments(send([video("6")]))[0], ref: null }]);
  assert.deepEqual([captured, foreign, sent, omitted, unrecorded, bare].map(videoStatus), [
    "Video is not available here", "Video is not available here", "Video not captured",
    "Video omitted from the recording", "Video data was not recorded", "Video data was not recorded",
  ]);
  // The host offers no address for the member (the app answers null for
  // another origin's): the bytes were recorded, and the tile does not say otherwise.
  const { markup } = renderRail([captured, foreign], recordingAssets({ attachmentAssetUrl: () => null }).assets);
  const drawn = figures(markup);
  assert.equal(drawn.length, 2);
  for (const figure of drawn) {
    assert.match(figure, /data-video="uncaptured"/);
    assert.match(figure, /<div class="tr-att__tile tr-att__tile--video tr-att__tile--missing" tabindex="-1" aria-label="Video is not available here — /);
    assert.match(figure, /<span>Video is not available here<\/span>/);
    assert.doesNotMatch(figure, /<button|<video|Retry|not recorded|not captured/);
  }
});

test("the tile's shape follows the dimensions, between a square and 21:9", () => {
  assert.equal(videoAspect(1920, 1080), 16 / 9);
  assert.equal(videoAspect(null, null), 16 / 9);
  assert.equal(videoAspect(0, 1080), 16 / 9);
  assert.equal(videoAspect(1440, 1080), 4 / 3);
  assert.equal(videoAspect(1080, 1920), 1, "a portrait video letterboxes in a square tile");
  assert.equal(videoAspect(4000, 100), 21 / 9);
  const [portrait] = messageAttachments(send([video("1", mp4(), { dimensions: { width: 1080, height: 1920 } })]));
  assert.match(face({ fact: portrait }), /style="--tr-video-aspect:1\.0000"/);
});

test("one rail draws images and videos in order, and every kind and state says something", () => {
  const { asked, assets } = recordingAssets();
  const facts = messageAttachments(send([
    image("1"), video("2"), uncaptured("3"), uncaptured("4", "image"),
    { ...video("5"), kind: "audio", name: "voice.m4a", source: reference("/w/voice.m4a") },
    video("6", mp4(), { source: { type: "unavailable", reason: "omitted" } }),
  ]));
  const { markup } = renderRail(facts, assets);
  const drawn = figures(markup);
  assert.equal(drawn.length, 6);
  assert.deepEqual(drawn.map((figure) => /data-video="([^"]+)"/.exec(figure)?.[1] ?? null), [null, "idle", "uncaptured", null, null, "uncaptured"]);
  assert.match(drawn[0], /<button type="button" class="tr-att__tile" aria-busy="false" aria-label="Open \[Image #1\] — shot-1\.png" aria-haspopup="dialog">/);
  assert.match(drawn[3], /<span>Image not captured<\/span>/);
  assert.doesNotMatch(drawn[3], /<button/);
  assert.match(drawn[4], /<span>Attachment preview unavailable<\/span>/);
  assert.match(drawn[5], /<span>Video omitted from the recording<\/span>/);
  assert.ok(asked.image.every(([mediaType]) => mediaType === "image/png"), "no video was handed to imageAssetUrl");
  assert.deepEqual(asked.media, []);
  assert.deepEqual(facts.map(attachmentStatus), [
    "Image data was not recorded", "Video is not available here", "Video not captured", "Image not captured",
    "Attachment preview unavailable", "Video omitted from the recording",
  ]);
  for (const kind of ["document", "audio", "file", "something-new"]) {
    assert.equal(attachmentStatus({ ...facts[0], kind }), "Attachment preview unavailable", kind);
  }
});

test("an unlabelled tile is numbered among its own kind, so the rail and the gallery call a picture the same", () => {
  const { assets } = recordingAssets();
  const bare = { label: null };
  const facts = messageAttachments(send([
    video("1", mp4(), { name: null }), image("2", png("2"), bare), uncaptured("3"), image("4", png("4"), bare),
    { ...video("5"), kind: "document", name: "notes.pdf", media_type: "application/pdf" }, uncaptured("6", "image"),
    video("7", mov(), { name: null }),
  ]));
  const drawn = figures(renderRail(facts, assets).markup);
  const named = (figure) => /aria-label="Open (Image \d+) — /.exec(figure)?.[1] ?? /title="((?:Image|Video|Attachment) \d+)\b/.exec(figure)?.[1];
  assert.deepEqual(drawn.map(named), ["Video 1", "Image 1", "Video 2", "Image 2", "Attachment 5", "Image 3", "Video 3"]);
  assert.match(drawn[1], /aria-label="Open Image 1 — shot-2\.png"/, "the first picture, though a video stands before it");
  assert.match(drawn[0], /aria-label="Play video — Video 1 · /, "a nameless video goes by its number among the videos");
  // The gallery the first picture opens numbers the same row's images the same way.
  const reader = createAttachmentReader(assets, refusedRead);
  const images = facts.filter((fact) => fact.kind === "image");
  const { images: slots } = attachmentGallery(reader, images[0], facts);
  assert.deepEqual(slots.map((slot) => slot.label), ["Image 1", "Image 2", "Image 3"]);
  assert.deepEqual(slots.map((slot) => slot.key), images.map((fact) => fact.key));
  assert.deepEqual(images.map((fact) => named(drawn[facts.indexOf(fact)])), slots.map((slot) => slot.label));
});

/* ── the demo ───────────────────────────────────────────────────────────── */

test("the demo holds one captured and one uncaptured video, and reads no encoded video", async () => {
  const demo = createRoomDemo(Date.UTC(2026, 10, 12, 12));
  const row = demo.timelines.get("s8").find((entry) => entry.body.type === "tool_use");
  const [captured, sent] = row.body.tool.attachments;
  assert.deepEqual([captured.kind, sent.kind], ["video", "video"]);
  assert.equal(isRecordedVideoAsset(captured.source.asset), true);
  assert.deepEqual(sent.source, { type: "reference", reference: { type: "file", path: "/Users/demo/Projects/rosetta/work/before-the-split.mov", resolved: true } });
  assert.deepEqual([sent.name, sent.media_type, sent.dimensions], ["before-the-split.mov", "video/quicktime", null]);
  assert.deepEqual(timelineImages(demo.timelines.get("s8")).map((fact) => [fact.kind, fact.missing]), [["video", false], ["video", true]]);
  const ports = createDemoPorts(demo);
  assert.equal(ports.assets.mediaAssetUrl, undefined, "the demo offers no video address: its bytes are read");
  const bytes = await ports.sessions.open("s8").readAsset(captured.source.asset);
  const text = (start, end) => String.fromCharCode(...bytes.subarray(start, end));
  assert.equal(bytes.byteLength, 64);
  assert.deepEqual([bytes[3], text(4, 8), text(8, 12), bytes[27], text(28, 32)], [24, "ftyp", "isom", 40, "free"], "a header and padding, no media");
  assert.ok(bytes.subarray(32).every((byte) => byte === 0));
});
