// Transcript attachments through the room's existing file-viewer seam.
//
// The viewer deliberately knows neither the daemon nor the transcript. This
// adapter turns a canonical content-addressed asset into the same `FileHandle`
// + `FileSource` pair the worktree supplies, so thumbnails and inline image
// markers open the established image view instead of navigating the page.

import type { AttachmentFact } from "../../lib/transcript";
import { client } from "../../lib/rosetta";
import { resolveKind } from "../file-viewer";
import type { FileHandle, FileSource } from "../file-viewer";

const EXTENSION_BY_MEDIA_TYPE: ReadonlyMap<string, string> = new Map([
  ["image/png", "png"],
  ["image/jpeg", "jpg"],
  ["image/jpg", "jpg"],
  ["image/gif", "gif"],
  ["image/webp", "webp"],
  ["image/avif", "avif"],
  ["image/bmp", "bmp"],
  ["image/x-icon", "ico"],
  ["image/apng", "apng"],
  ["image/svg+xml", "svg"],
]);

/** The URL of bytes the daemon kept, never a provider-authored path. */
export function attachmentAssetUrl(fact: AttachmentFact): string | null {
  const source = fact.ref?.source;
  if (source === undefined || source === null || source.type !== "asset") return null;
  try {
    const url = new URL(source.asset.url, `${client.baseUrl}/`);
    return (url.protocol === "http:" || url.protocol === "https:") && url.origin === new URL(client.baseUrl).origin ? url.toString() : null;
  } catch {
    return null;
  }
}

function viewerName(fact: AttachmentFact): string {
  const source = fact.ref?.source;
  const base =
    fact.name?.trim() ||
    fact.ref?.label?.trim() ||
    "Image";
  const kind = resolveKind(base).viewer.id;
  if (kind === "image" || kind === "svg") return base;
  const mediaType = fact.mediaType ?? (source?.type === "asset" ? source.asset.media_type : null);
  const extension = mediaType === null ? undefined : EXTENSION_BY_MEDIA_TYPE.get(mediaType.toLowerCase());
  return extension === undefined ? base : `${base}.${extension}`;
}

/** A stored transcript asset as the generic file viewer understands it. */
export function handleForAttachment(fact: AttachmentFact): FileHandle | null {
  const source = fact.ref?.source;
  const url = attachmentAssetUrl(fact);
  if (source === undefined || source === null || source.type !== "asset" || url === null) return null;
  return {
    key: `attachment:${source.asset.digest}`,
    name: viewerName(fact),
    path: null,
    size: fact.byteLen ?? source.asset.byte_len,
    ref: source.asset,
  };
}

export function canOpenAttachment(fact: AttachmentFact): boolean {
  return fact.kind === "image" && handleForAttachment(fact) !== null;
}

/** Stable by construction: `useFileContent` treats source identity as a read. */
export const attachmentFileSource: FileSource = {
  async read(handle: FileHandle, signal: AbortSignal): Promise<Uint8Array> {
    const asset = handle.ref as import("@rosetta/protocol").AssetRef;
    if (!asset || typeof asset.url !== "string") throw new Error("that attachment is no longer available");
    return client.readAsset(asset, signal);
  },
};
