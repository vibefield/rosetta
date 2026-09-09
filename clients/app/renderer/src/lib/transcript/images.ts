// Image occurrences from canonical activity, independent of either fold level.
// Reuse the detail projection's exact file addresses, especially workflow indices.
import type { ResourceRef, TimelineEntry } from "@rosetta/protocol";
import { projectAction } from "./families.ts";
import type { PathFact } from "./families.ts";
import type { AttachmentFact, LedgerMember } from "./turns.ts";
import { readAttachments } from "./turns.ts";
import { resourceLabel, splitPath } from "./vocabulary.ts";

const IMAGE_TYPES: Readonly<Record<string, string>> = {
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
  webp: "image/webp", avif: "image/avif", bmp: "image/bmp", ico: "image/x-icon",
  apng: "image/apng", svg: "image/svg+xml",
};

function resourcePath(resource: ResourceRef): PathFact {
  const label = resourceLabel(resource);
  return { label, ...splitPath(label), resource };
}

function rowImages(row: TimelineEntry): AttachmentFact[] {
  if (row.body.type !== "tool_use") return [];
  const { action, native } = row.body.tool;
  if (!["file_read", "artifact", "workflow", "structured_output", "external"].includes(action.type)) return [];
  const detail = projectAction(action, native, {}, { sessionId: row.session_id, timelineId: row.timeline_id }).detail;
  const paths = detail.kind === "paths" ? detail.paths
    : detail.kind === "workflow" ? [detail.workflow.script, ...detail.workflow.resources].filter((path): path is PathFact => path !== null)
    : detail.kind === "fields" ? detail.resources
    : detail.kind === "document" && detail.document.type === "inline" ? detail.document.resources.map(resourcePath)
    : [];
  return paths.flatMap((path, index) => {
    const resource = path.resource;
    if (resource.type !== "file" && resource.type !== "asset" && resource.type !== "url") return [];
    const asset = resource.type === "asset" ? resource.asset : null;
    const metadata = action.type === "artifact" ? action.media : null;
    const declaredType = asset?.media_type ?? metadata?.media_type ?? null;
    const extension = path.name.split(".").at(-1)?.toLowerCase() ?? "";
    const mediaType = declaredType?.startsWith("image/") ? declaredType : IMAGE_TYPES[extension] ?? null;
    const explicitImage = action.type === "file_read" ? action.targets[index]?.media_kind === "image"
      : action.type === "artifact" && action.artifact_kind === "image";
    if (mediaType === null && !explicitImage) return [];
    const key = `${row.session_id}:${row.timeline_id}:image:${path.read?.targetIndex ?? index}`;
    const ref = {
      id: key, kind: "image" as const, label: null, name: path.name || null,
      media_type: mediaType, byte_len: asset?.byte_len ?? null, dimensions: null, message_ranges: [],
      source: asset ? { type: "asset" as const, asset } : { type: "reference" as const, reference: resource },
    };
    const fact = readAttachments(null, [ref]).attachments[0];
    return [{ ...fact, width: metadata?.width ?? null, height: metadata?.height ?? null,
      ...(path.read ? { readPath: path, missing: false, caption: "Current file" } : {}) }];
  });
}

/** Keep occurrences in transcript order, including repeated reads of one image. */
export function activityImages(members: readonly LedgerMember[]): AttachmentFact[] {
  return members.flatMap(member => member.kind === "line" ? member.line.rows.flatMap(rowImages) : []);
}
