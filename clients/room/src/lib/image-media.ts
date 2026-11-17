import type { AssetRef } from "@vibefield/rosetta-protocol";

/** File-format hints for image targets without a filename extension or MIME type.
 * This only selects the existing viewer; the browser still validates/decodes it. */
export function imageMediaTypeFromBytes(bytes: Uint8Array): string | null {
  const starts = (...prefix: number[]) => prefix.every((value, index) => bytes[index] === value);
  const text = (start: number, length: number) => String.fromCharCode(...bytes.subarray(start, start + length));
  if (starts(137, 80, 78, 71, 13, 10, 26, 10)) return "image/png";
  if (starts(255, 216, 255)) return "image/jpeg";
  if (["GIF87a", "GIF89a"].includes(text(0, 6))) return "image/gif";
  if (text(0, 4) === "RIFF" && text(8, 4) === "WEBP") return "image/webp";
  if (text(0, 2) === "BM") return "image/bmp";
  if (starts(0, 0, 1, 0)) return "image/x-icon";
  if (text(4, 4) === "ftyp" && ["avif", "avis"].includes(text(8, 4))) return "image/avif";
  if (/^\s*(?:<\?xml[^>]*>\s*)?<svg[\s>]/i.test(new TextDecoder().decode(bytes.subarray(0, 512)))) return "image/svg+xml";
  return null;
}

/** The six recorded image formats ingestion keeps as verified originals, each
 * with the one extension its content-addressed member name carries. */
const RECORDED_IMAGE_EXTENSION: ReadonlyMap<string, string> = new Map([
  ["image/png", "png"],
  ["image/jpeg", "jpg"],
  ["image/gif", "gif"],
  ["image/webp", "webp"],
  ["image/bmp", "bmp"],
  ["image/x-icon", "ico"],
]);

const BUNDLE_VARIANTS = ["rail", "tile", "tile2x", "preview"] as const;

/** The member extension of a recorded image format, or undefined for any
 * other media type. */
export function recordedImageExtension(mediaType: string): string | undefined {
  return RECORDED_IMAGE_EXTENSION.get(mediaType);
}

/** One content-addressed projection member: `/api/assets/<digest>.<extension>`. */
function isMember(value: AssetRef | undefined, mediaType: string, extension: string): boolean {
  return value?.media_type === mediaType && value.byte_len > 0 && /^[a-f0-9]{64}$/.test(value.digest)
    && value.url === `/api/assets/${value.digest}.${extension}`;
}

/** A complete WebP bundle: the member and its four renditions, all encoded
 * by ingestion before deferred renditions (projections built earlier). */
export function isLegacyImageBundle(asset: AssetRef): boolean {
  const member = (value: AssetRef | undefined) => isMember(value, "image/webp", "webp");
  return member(asset) && BUNDLE_VARIANTS.every(variant => member(asset.image_variants?.[variant]));
}

/** A transcript image the room may show: a recorded original (a member in one
 * of the six formats with no rendition map) or a complete legacy WebP bundle.
 * One transcript may hold both. Either is drawn only through the daemon's
 * WebP renditions (`AssetSource.imageAssetUrl`); the room never draws the
 * recorded bytes themselves. */
export function isRecordedImageAsset(asset: AssetRef): boolean {
  // A malformed row's missing asset is refused, never a render-time throw.
  if (asset == null) return false;
  if (asset.image_variants != null) return isLegacyImageBundle(asset);
  const extension = recordedImageExtension(asset.media_type);
  return extension !== undefined && isMember(asset, asset.media_type, extension);
}

/** The two video containers ingestion records, each with the one extension
 * its content-addressed member name carries. */
const RECORDED_VIDEO_EXTENSION: ReadonlyMap<string, string> = new Map([
  ["video/mp4", "mp4"],
  ["video/quicktime", "mov"],
]);

/** A sent video the room may play: one recorded member,
 * `/api/assets/<digest>.mp4` or `.mov`, with no rendition map. The room never
 * draws it as an image and asks no rendition of it; `VideoTile` plays the
 * member's own bytes once the reader asks. */
export function isRecordedVideoAsset(asset: AssetRef): boolean {
  // A malformed row's missing asset is refused, never a render-time throw.
  if (asset == null || asset.image_variants != null) return false;
  const extension = RECORDED_VIDEO_EXTENSION.get(asset.media_type);
  return extension !== undefined && isMember(asset, asset.media_type, extension);
}
