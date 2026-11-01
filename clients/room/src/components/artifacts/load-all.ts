import { ApiRequestError, type ArtifactListRequest, type ArtifactPage } from "@vibefield/rosetta-protocol";
import type { CatalogPort } from "../../ports";

/** Drain the bounded API pages. A source update may invalidate a scan; start
 * over without mixing epochs, and keep the current cards visible until then. */
export async function loadAllArtifacts(port: NonNullable<CatalogPort["artifacts"]>, request: ArtifactListRequest, signal: AbortSignal, update: (page: ArtifactPage) => void): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    const entries = new Map<string, ArtifactPage["entries"][number]>();
    const cursors = new Set<string>();
    let cursor: string | null = null;
    try {
      do {
        signal.throwIfAborted();
        const page = await port.list({ ...request, limit: 100, cursor }, signal);
        signal.throwIfAborted();
        for (const entry of page.entries) entries.set(entry.publication.id, entry);
        update({ ...page, entries: [...entries.values()] });
        cursor = page.next_cursor;
        if (cursor !== null && cursors.has(cursor)) throw new Error("The artifact list returned a repeated page. Refresh to try again.");
        if (cursor !== null) cursors.add(cursor);
      } while (cursor !== null);
      return;
    } catch (error) {
      signal.throwIfAborted();
      if (!(error instanceof ApiRequestError && error.code === "cursor_expired") || attempt >= 2) throw error;
    }
  }
}
