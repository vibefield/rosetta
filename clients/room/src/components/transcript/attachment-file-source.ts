// Transcript attachments through the room's existing file-viewer seam.
//
// The viewer deliberately knows neither the daemon nor the transcript. This
// adapter turns a canonical content-addressed asset into the same `FileHandle`
// + `FileSource` pair the worktree supplies, so thumbnails and inline image
// markers open the established image view instead of navigating the page.
// Addresses come from the host's asset source; bytes ride the reading
// session's client.

import { useMemo } from "react";
import type { AssetRef } from "@vibefield/rosetta-protocol";
import type { AttachmentFact } from "../../lib/transcript";
import type { AssetSource } from "../../ports";
import { useReadingSession, useRoom } from "../../room-context";
import { resolveKind } from "../file-viewer";
import type { FileHandle, FileSource } from "../file-viewer";
import type { ImageResource } from "../file-viewer/types";
import { isOptimizedImageAsset } from "../../lib/image-media";

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

/** Reads one stored member's bytes. */
export type AssetReader = (asset: AssetRef, signal: AbortSignal) => Promise<Uint8Array>;

export interface AttachmentReader {
  /** The address of bytes the daemon kept, never a provider-authored path. */
  assetUrl(fact: AttachmentFact): string | null;
  imageResource(fact: AttachmentFact): ImageResource | null;
  /** A stored transcript asset as the generic file viewer understands it. */
  handleFor(fact: AttachmentFact): FileHandle | null;
  canOpen(fact: AttachmentFact): boolean;
  /** Stable by construction: `useFileContent` treats source identity as a read. */
  readonly fileSource: FileSource;
  readonly readAsset: AssetReader;
}

export function attachmentViewerName(fact: AttachmentFact): string {
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

/** One stored image as the viewer asks for it: by rendition, through the host. */
function imageOf(assets: AssetSource, asset: AssetRef, width?: number, height?: number): ImageResource {
  return { key: asset.digest, url: (rendition) => assets.imageAssetUrl(asset, rendition), width, height };
}

export function createAttachmentReader(assets: AssetSource, readAsset: AssetReader): AttachmentReader {
  function assetUrl(fact: AttachmentFact): string | null {
    const source = fact.ref?.source;
    if (source === undefined || source === null || source.type !== "asset") return null;
    if (fact.kind === "image" && !isOptimizedImageAsset(source.asset)) return null;
    return assets.attachmentAssetUrl(source.asset);
  }

  function imageResource(fact: AttachmentFact): ImageResource | null {
    const source = fact.ref?.source;
    const asset = source?.type === "asset" && assetUrl(fact) !== null ? source.asset : null;
    if (!asset || !isOptimizedImageAsset(asset)) return null;
    return imageOf(assets, asset, fact.ref?.dimensions?.width, fact.ref?.dimensions?.height);
  }

  function handleFor(fact: AttachmentFact): FileHandle | null {
    const source = fact.ref?.source;
    const url = assetUrl(fact);
    if (source === undefined || source === null || source.type !== "asset" || url === null) return null;
    return {
      key: `attachment:${source.asset.digest}`,
      name: attachmentViewerName(fact),
      path: null,
      size: fact.byteLen ?? source.asset.byte_len,
      ref: source.asset,
    };
  }

  const fileSource: FileSource = {
    image(handle) {
      const asset = handle.ref as AssetRef;
      if (!asset || !isOptimizedImageAsset(asset)) return null;
      return imageOf(assets, asset);
    },
    async read(handle: FileHandle, signal: AbortSignal): Promise<Uint8Array> {
      const asset = handle.ref as AssetRef;
      if (!asset || !isOptimizedImageAsset(asset)) throw new Error("that attachment is no longer available");
      return readAsset(asset, signal);
    },
  };

  return {
    assetUrl,
    imageResource,
    handleFor,
    canOpen: (fact) => fact.kind === "image" && handleFor(fact) !== null,
    fileSource,
    readAsset,
  };
}

/** The attachment reader for the document being read here: the host's
 * addresses, and bytes through the reading session's client. */
export function useAttachmentReader(): AttachmentReader {
  const { assets, sessions } = useRoom();
  const sessionId = useReadingSession();
  return useMemo(() => createAttachmentReader(assets, (asset, signal) => {
    if (sessionId === null) return Promise.reject(new Error("No session is open to read that attachment"));
    return sessions.run(sessionId, (client) => client.readAsset(asset, signal));
  }), [assets, sessions, sessionId]);
}
