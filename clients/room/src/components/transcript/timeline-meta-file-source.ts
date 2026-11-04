// Lazy timeline_meta resources through the room's shared file viewer.
// The metadata frame carries only safe canonical references; bytes are read
// after the reader presses a plan or artifact row and then ride on the handle
// so the viewer never repeats a mutable filesystem read. A recorded image
// member is the exception: it is drawn through the host's renditions, as a
// transcript image is, and its recorded bytes are never read for display.

import type { AssetRef, TimelineMetaArtifact, TimelineMetaPlan } from "@vibefield/rosetta-protocol";
import { isRecordedImageAsset, recordedImageExtension } from "../../lib/image-media";
import type { SessionLeases } from "../../lib/session-leases";
import type { AssetSource } from "../../ports";
import { resolveKind } from "../file-viewer";
import type { FileHandle, FileSource } from "../file-viewer";
import type { ImageResource } from "../file-viewer/types";
import { imageOf } from "./attachment-file-source";

interface TimelineMetaFilePayload {
  readonly type: "timeline-meta-file";
  readonly bytes: Uint8Array;
}

interface TimelineMetaImagePayload {
  readonly type: "timeline-meta-image";
  readonly image: ImageResource;
}

const EXTENSION_BY_MEDIA_TYPE: ReadonlyMap<string, string> = new Map([
  ["image/png", "png"],
  ["image/jpeg", "jpg"],
  ["image/gif", "gif"],
  ["image/webp", "webp"],
  ["image/bmp", "bmp"],
  ["image/x-icon", "ico"],
  ["image/avif", "avif"],
  ["image/svg+xml", "svg"],
  ["application/json", "json"],
  ["text/markdown", "md"],
  ["text/plain", "txt"],
  ["text/html", "html"],
]);

function nameWithType(name: string, mediaType: string | null): string {
  if (resolveKind(name).viewer.id !== "binary" || mediaType === null) return name;
  const extension = EXTENSION_BY_MEDIA_TYPE.get(mediaType.toLowerCase().split(";")[0]!.trim());
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
  sessions: SessionLeases,
  sessionId: string,
  plan: TimelineMetaPlan,
): Promise<FileHandle> {
  const bytes = await sessions.run(sessionId, (client) => client.readPlan(sessionId, plan.source));
  return handle(`timeline-meta:plan:${sessionId}:${plan.id}`, plan.name, null, bytes);
}

/** A recorded image member by its renditions: the name carries the image's
 * own extension so the viewer opens it as an image, and nothing is read. */
function imageHandle(key: string, name: string, asset: AssetRef, extension: string, assets: AssetSource): FileHandle {
  return {
    key,
    name: resolveKind(name).viewer.id === "image" ? name : `${name}.${extension}`,
    path: null,
    size: asset.byte_len,
    ref: { type: "timeline-meta-image", image: imageOf(assets, asset) } satisfies TimelineMetaImagePayload,
  };
}

export async function handleForTimelineMetaArtifact(
  sessions: SessionLeases,
  assets: AssetSource,
  sessionId: string,
  artifact: TimelineMetaArtifact,
): Promise<FileHandle> {
  const source = artifact.source;
  if (source.type === "html_publication") {
    if (source.availability !== "captured") throw new Error("This artifact's HTML was not captured. Its publication remains in the session.");
    const bytes = await sessions.run(sessionId, (client) => {
      if (!client.readArtifact) throw new Error("This host cannot read captured artifacts");
      return client.readArtifact(sessionId, source.publication_id);
    });
    return handle(`html-artifact:${source.publication_id}`, source.filename, null, bytes);
  }
  if (source.type === "timeline_file") {
    const bytes = await sessions.run(sessionId, (client) => client.readFileTarget(
      sessionId,
      source.timeline_id,
      source.target_index,
    ));
    return handle(
      `timeline-meta:artifact:${sessionId}:${artifact.id}`,
      source.path.split(/[\\/]/).at(-1) || nameWithType(artifact.name, artifact.media_type),
      source.path,
      bytes,
    );
  }

  const key = `timeline-meta:artifact:${sessionId}:${artifact.id}`;
  const extension = recordedImageExtension(source.asset.media_type);
  if (extension !== undefined) {
    // Recorded image bytes are opened or saved, never drawn.
    if (!isRecordedImageAsset(source.asset)) throw new Error("This artifact's image is not a recorded member");
    return imageHandle(key, artifact.name, source.asset, extension, assets);
  }
  const bytes = await sessions.run(sessionId, (client) => client.readAsset(source.asset));
  return handle(
    key,
    nameWithType(artifact.name, artifact.media_type ?? source.asset.media_type),
    null,
    bytes,
  );
}

export const timelineMetaFileSource: FileSource = {
  image(file: FileHandle): ImageResource | null {
    const payload = file.ref as Partial<TimelineMetaImagePayload> | undefined;
    return payload?.type === "timeline-meta-image" && payload.image ? payload.image : null;
  },
  async read(file: FileHandle, signal: AbortSignal): Promise<Uint8Array> {
    if (signal.aborted) throw new DOMException("the file open was cancelled", "AbortError");
    const payload = file.ref as Partial<TimelineMetaFilePayload> | undefined;
    if (payload?.type !== "timeline-meta-file" || !(payload.bytes instanceof Uint8Array)) {
      throw new Error("that timeline resource is no longer available");
    }
    return payload.bytes;
  },
};
