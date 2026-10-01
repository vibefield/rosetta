import type { AttachmentFact } from "../../lib/transcript";
import { client } from "../../lib/rosetta";
import type { FileSource } from "../file-viewer";
import type { GalleryImage } from "../file-viewer/ImageGalleryDialog";
import { attachmentImageResource, attachmentViewerName, canOpenAttachment } from "./attachment-file-source";

export type AttachmentOpener = (fact: AttachmentFact, row?: readonly AttachmentFact[]) => void;

/** Snapshot just the row that was opened, including distinct uses of one asset. */
export function attachmentGallery(fact: AttachmentFact, row: readonly AttachmentFact[] = [fact]) {
  const images: GalleryImage[] = row.filter(item => item.kind === "image").map((item, index) => {
    const image = item.key === fact.key ? fact : item;
    const asset = image.ref?.source.type === "asset" ? image.ref.source.asset : null;
    return {
      key: image.key,
      label: image.ref?.label ?? `Image ${index + 1}`,
      handle: canOpenAttachment(image) ? {
        key: image.key, name: attachmentViewerName(image),
        path: image.readPath?.resource.type === "file" ? image.readPath.resource.path : null,
        size: image.byteLen ?? asset?.byte_len ?? -1,
        ref: image,
      } : null,
    };
  });
  return { images, initialKey: fact.key };
}

/** Each read is an exact canonical target or a stored asset, never a guessed path. */
export const attachmentGallerySource: FileSource = {
  image(handle) { return attachmentImageResource(handle.ref as AttachmentFact); },
  async read(handle, signal) {
    const fact = handle.ref as AttachmentFact;
    if (signal.aborted) throw new DOMException("Image read cancelled", "AbortError");
    const address = fact.readPath?.read;
    if (address) return client.readFileTarget(address.sessionId, address.timelineId, address.targetIndex, signal);
    if (canOpenAttachment(fact) && fact.ref?.source.type === "asset") return client.readAsset(fact.ref.source.asset, signal);
    throw new Error("Image data was not recorded");
  },
};
