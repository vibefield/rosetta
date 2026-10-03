// Image occurrences from canonical activity, independent of either fold level.
// Every displayed image comes from recorded, optimized canonical members.
import type { ResourceRef, TimelineEntry } from "@rosetta/protocol";
import { projectAction } from "./families";
import type { PathFact } from "./families";
import type { AttachmentFact, LedgerMember } from "./turns";
import { readAttachments } from "./turns";
import { resourceLabel, splitPath } from "./vocabulary";
import { isOptimizedImageAsset } from "../image-media";

function resourcePath(resource: ResourceRef): PathFact {
  const label = resourceLabel(resource);
  return { label, ...splitPath(label), resource };
}

function rowImages(row: TimelineEntry): AttachmentFact[] {
  if (row.body.type !== "tool_use") return [];
  const { action, native } = row.body.tool;
  const recorded = messageAttachments(row).filter(fact => fact.kind === "image");
  if (!["artifact", "workflow", "structured_output", "external"].includes(action.type)) return recorded;
  const detail = projectAction(action, native, {}, { sessionId: row.session_id, timelineId: row.timeline_id }).detail;
  const paths = detail.kind === "paths" ? detail.paths
    : detail.kind === "workflow" ? [detail.workflow.script, ...detail.workflow.resources].filter((path): path is PathFact => path !== null)
    : detail.kind === "fields" ? detail.resources
    : detail.kind === "document" && detail.document.type === "inline" ? detail.document.resources.map(resourcePath)
    : [];
  return [...recorded, ...paths.flatMap((path, index) => {
    const resource = path.resource;
    if (resource.type !== "asset" || !isOptimizedImageAsset(resource.asset)) return [];
    const asset = resource.asset;
    const key = `${row.session_id}:${row.timeline_id}:resource:${index}`;
    const ref = {
      id: key, kind: "image" as const, label: null, name: path.name || null,
      media_type: asset.media_type, byte_len: asset.byte_len, dimensions: null, message_ranges: [],
      source: { type: "asset" as const, asset },
    };
    return readAttachments(null, [ref]).attachments;
  })];
}

/** Keep occurrences in transcript order, including repeated reads of one image. */
export function activityImages(members: readonly LedgerMember[]): AttachmentFact[] {
  return members.flatMap(member => member.kind === "line" ? member.line.rows.flatMap(rowImages) : []);
}

/** Attachment ids belong to a message, not to the whole transcript. */
export function messageAttachments(row: TimelineEntry): AttachmentFact[] {
  const refs = row.body.type === "user" || row.body.type === "agent" ? row.body.attachments
    : row.body.type === "tool_use" ? row.body.tool.attachments ?? []
    : row.body.type === "collaboration" ? row.body.event.attachments ?? [] : [];
  return readAttachments(null, refs).attachments.map(fact => ({
    ...fact, key: `${row.session_id}:${row.timeline_id}:attachment:${fact.key}`,
  }));
}

/** One rail for every image occurrence in the run, regardless of row type. */
export function timelineImages(rows: readonly TimelineEntry[]): AttachmentFact[] {
  const seen = new Set<string>();
  return rows.flatMap(row => {
    const identity = JSON.stringify([row.session_id, row.timeline_id]);
    if (seen.has(identity)) return [];
    seen.add(identity);
    return row.body.type === "tool_use" ? rowImages(row)
      : messageAttachments(row).filter(fact => fact.kind === "image");
  });
}
