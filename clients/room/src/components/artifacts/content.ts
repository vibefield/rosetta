import type { ArtifactEntry } from "@vibefield/rosetta-protocol";
import type { CatalogPort } from "../../ports";
import { ARTIFACT_MAX_BYTES } from "./preview";

/** One open library's bounded cache for explicitly opened full previews. */
export class ArtifactContentCache {
  private readonly entries = new Map<string, Uint8Array>();
  private size = 0;

  async read(port: NonNullable<CatalogPort["artifacts"]>, entry: ArtifactEntry, signal: AbortSignal): Promise<Uint8Array> {
    const key = `${entry.publication.id}:${entry.asset?.digest ?? ""}`;
    const cached = this.entries.get(key);
    if (cached) return cached;
    const bytes = await port.read(entry.publication.session_id, entry.publication.id, signal);
    if (signal.aborted) throw new DOMException("Aborted", "AbortError");
    if (bytes.byteLength > ARTIFACT_MAX_BYTES) throw new Error("The captured HTML exceeds the preview limit");
    if (!this.entries.has(key)) {
      this.entries.set(key, bytes);
      this.size += bytes.byteLength;
      while (this.entries.size > 20 || this.size > 32 * 1024 * 1024) {
        const first = this.entries.keys().next().value!;
        this.size -= this.entries.get(first)!.byteLength;
        this.entries.delete(first);
      }
    }
    return bytes;
  }
}
