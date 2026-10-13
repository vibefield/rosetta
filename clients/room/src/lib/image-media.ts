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
/** Transcript images must be committed WebP bundles produced by ingestion. */
export function isOptimizedImageAsset(asset: AssetRef): boolean {
  const member = (value: AssetRef | undefined) => value?.media_type === "image/webp"
    && value.byte_len > 0 && /^[a-f0-9]{64}$/.test(value.digest)
    && value.url === `/api/assets/${value.digest}.webp`;
  return member(asset) && ["rail", "tile", "tile2x", "preview"].every(variant =>
    member(asset.image_variants?.[variant as "rail" | "tile" | "tile2x" | "preview"]));
}
