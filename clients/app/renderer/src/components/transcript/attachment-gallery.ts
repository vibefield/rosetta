import type { AttachmentFact } from "../../lib/transcript";
import type { FileSource } from "../file-viewer";
import type { GalleryImage } from "../file-viewer/ImageGalleryDialog";
import { attachmentViewerName } from "./attachment-file-source";
import type { AttachmentReader } from "./attachment-file-source";

export type AttachmentOpener = (fact: AttachmentFact, row?: readonly AttachmentFact[]) => void;

/** Snapshot just the row that was opened, including distinct uses of one asset. */
export function attachmentGallery(reader: AttachmentReader, fact: AttachmentFact, row: readonly AttachmentFact[] = [fact]) {
  const images: GalleryImage[] = row.filter(item => item.kind === "image").map((item, index) => {
    const image = item.key === fact.key ? fact : item;
    const asset = image.ref?.source.type === "asset" ? image.ref.source.asset : null;
    return {
      key: image.key,
      label: image.ref?.label ?? `Image ${index + 1}`,
      handle: reader.canOpen(image) ? {
        key: image.key, name: attachmentViewerName(image),
        path: null,
        size: image.byteLen ?? asset?.byte_len ?? -1,
        ref: image,
      } : null,
    };
  });
  return { images, initialKey: fact.key };
}

/** Transcript galleries read only optimized members committed during ingestion. */
export function createAttachmentGallerySource(reader: AttachmentReader): FileSource {
  return {
    image(handle) { return reader.imageResource(handle.ref as AttachmentFact); },
    async read(handle, signal) {
      const fact = handle.ref as AttachmentFact;
      if (signal.aborted) throw new DOMException("Image read cancelled", "AbortError");
      if (reader.canOpen(fact) && fact.ref?.source.type === "asset") return reader.readAsset(fact.ref.source.asset, signal);
      throw new Error("Image data was not recorded");
    },
  };
}
