// Canonical file-read targets through the shared file-viewer seam.
//
// A transcript path is evidence, not authority. The renderer therefore sends
// only the session id, timeline id and target index back to rosettad. The
// daemon re-reads the canonical row, resolves the path recorded there and
// returns a bounded payload. The bytes ride on the handle so opening the
// dialog does not issue a second filesystem read against a file that may have
// changed between requests.

import type { PathFact } from "../../lib/transcript";
import { client } from "../../lib/rosetta";
import type { FileHandle, FileSource } from "../file-viewer";

interface ReadFilePayload {
  readonly type: "transcript-read";
  readonly bytes: Uint8Array;
}

/** Resolve and read one exact canonical target before opening the viewer. */
export async function handleForReadPath(path: PathFact): Promise<FileHandle | null> {
  const address = path.read;
  if (address === undefined || path.resource.type !== "file") return null;
  const bytes = await client.readFileTarget(
    address.sessionId,
    address.timelineId,
    address.targetIndex,
  );
  return {
    key: `read:${address.sessionId}:${address.timelineId}:${address.targetIndex}`,
    name: path.name || "file",
    path: path.resource.path,
    size: bytes.byteLength,
    ref: { type: "transcript-read", bytes } satisfies ReadFilePayload,
  };
}

/** Stable by construction: `useFileContent` treats source identity as a read. */
export const transcriptReadFileSource: FileSource = {
  async read(handle: FileHandle, signal: AbortSignal): Promise<Uint8Array> {
    if (signal.aborted) throw new DOMException("the file open was cancelled", "AbortError");
    const payload = handle.ref as Partial<ReadFilePayload> | undefined;
    if (payload?.type !== "transcript-read" || !(payload.bytes instanceof Uint8Array)) {
      throw new Error("that recorded file is no longer available");
    }
    return payload.bytes;
  },
};
