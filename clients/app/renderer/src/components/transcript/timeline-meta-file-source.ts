// Lazy timeline_meta resources through the room's shared file viewer.
// The metadata frame carries only safe canonical references; bytes are read
// after the reader presses a plan or artifact row and then ride on the handle
// so the viewer never repeats a mutable filesystem read.

import type { TimelineMetaArtifact, TimelineMetaPlan } from "@rosetta/protocol";
import { client } from "../../lib/rosetta";
import { resolveKind } from "../file-viewer";
import type { FileHandle, FileSource } from "../file-viewer";

interface TimelineMetaFilePayload {
  readonly type: "timeline-meta-file";
  readonly bytes: Uint8Array;
}

const EXTENSION_BY_MEDIA_TYPE: ReadonlyMap<string, string> = new Map([
  ["image/png", "png"],
  ["image/jpeg", "jpg"],
  ["image/gif", "gif"],
  ["image/webp", "webp"],
  ["image/avif", "avif"],
  ["image/svg+xml", "svg"],
  ["application/json", "json"],
  ["text/markdown", "md"],
  ["text/plain", "txt"],
]);

function nameWithType(name: string, mediaType: string | null): string {
  if (resolveKind(name).viewer.id !== "binary" || mediaType === null) return name;
  const extension = EXTENSION_BY_MEDIA_TYPE.get(mediaType.toLowerCase());
  return extension === undefined ? name : `${name}.${extension}`;
}

function handle(
  key: string,
  name: string,
  path: string | null,
  bytes: Uint8Array,
): FileHandle {
  return {
    key,
    name,
    path,
    size: bytes.byteLength,
    ref: { type: "timeline-meta-file", bytes } satisfies TimelineMetaFilePayload,
  };
}

export async function handleForTimelineMetaPlan(
  sessionId: string,
  plan: TimelineMetaPlan,
): Promise<FileHandle> {
  const bytes = await client.readPlan(sessionId, plan.source);
  return handle(`timeline-meta:plan:${sessionId}:${plan.id}`, plan.name, null, bytes);
}

export async function handleForTimelineMetaArtifact(
  sessionId: string,
  artifact: TimelineMetaArtifact,
): Promise<FileHandle> {
  const source = artifact.source;
  if (source.type === "timeline_file") {
    const bytes = await client.readFileTarget(
      sessionId,
      source.timeline_id,
      source.target_index,
    );
    return handle(
      `timeline-meta:artifact:${sessionId}:${artifact.id}`,
      nameWithType(artifact.name, artifact.media_type),
      source.path,
      bytes,
    );
  }

  let url: URL;
  try {
    url = new URL(source.asset.url, `${client.baseUrl}/`);
  } catch {
    throw new Error("that stored artifact is no longer available");
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("that stored artifact has an unsupported address");
  }
  const response = await fetch(url);
  if (!response.ok) throw new Error(`artifact read failed · HTTP ${response.status}`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  return handle(
    `timeline-meta:artifact:${sessionId}:${artifact.id}`,
    nameWithType(artifact.name, artifact.media_type ?? source.asset.media_type),
    null,
    bytes,
  );
}

export const timelineMetaFileSource: FileSource = {
  async read(file: FileHandle, signal: AbortSignal): Promise<Uint8Array> {
    if (signal.aborted) throw new DOMException("the file open was cancelled", "AbortError");
    const payload = file.ref as Partial<TimelineMetaFilePayload> | undefined;
    if (payload?.type !== "timeline-meta-file" || !(payload.bytes instanceof Uint8Array)) {
      throw new Error("that timeline resource is no longer available");
    }
    return payload.bytes;
  },
};
