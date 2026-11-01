import type { ArtifactEntry } from "@vibefield/rosetta-protocol";
import type { CatalogPort, HostPort } from "../../ports";
import { ARTIFACT_MAX_BYTES, artifactDocument } from "./preview";

type LibraryPort = NonNullable<CatalogPort["artifacts"]>;
type Renderer = NonNullable<HostPort["artifactThumbnail"]>;

/** One request at a time, including the HTML fetch. The grid only retains PNGs. */
export class ArtifactThumbnailCache {
  private readonly images = new Map<string, string>();
  private size = 0;
  private tail: Promise<unknown> = Promise.resolve();

  read(port: LibraryPort, entry: ArtifactEntry, render: Renderer, signal: AbortSignal): Promise<string | null> {
    const key = entry.asset?.digest ?? entry.publication.id;
    const task = this.tail.then(async () => {
      signal.throwIfAborted();
      const cached = this.images.get(key);
      if (cached) return cached;
      const bytes = await port.read(entry.publication.session_id, entry.publication.id, signal);
      signal.throwIfAborted();
      if (bytes.byteLength > ARTIFACT_MAX_BYTES) return null;
      const image = await render(artifactDocument(new TextDecoder().decode(bytes)));
      if (image?.startsWith("data:image/png;base64,") && image.length <= 3 * 1024 * 1024) {
        this.images.set(key, image);
        this.size += image.length;
        while (this.images.size > 256 || this.size > 16 * 1024 * 1024) {
          const oldest = this.images.keys().next().value!;
          this.size -= this.images.get(oldest)!.length;
          this.images.delete(oldest);
        }
        return image;
      }
      return null;
    });
    this.tail = task.catch(() => {});
    return task;
  }
}

const caches = new WeakMap<LibraryPort, ArtifactThumbnailCache>();
export function artifactThumbnails(port: LibraryPort): ArtifactThumbnailCache {
  let cache = caches.get(port);
  if (!cache) { cache = new ArtifactThumbnailCache(); caches.set(port, cache); }
  return cache;
}
