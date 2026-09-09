import type { CanonicalText } from "@rosetta/protocol";
import type { AttachmentFact } from "../../lib/transcript";
import type { FileHandle, FileSource } from "../file-viewer/types.ts";

export interface FullMessage {
  readonly key: string;
  readonly role: "user" | "agent";
  readonly text: string;
  readonly recordedText?: CanonicalText | null;
  readonly attachments?: readonly AttachmentFact[];
}

interface MessagePayload {
  readonly type: "transcript-message";
  readonly bytes: Uint8Array;
}

/** A full canonical member, or a snapshot of complete inline text. */
export function handleForMessage(message: FullMessage): FileHandle {
  const member = message.recordedText?.type === "member" ? message.recordedText : null;
  if (member !== null) return {
    key: `message:${message.key}:${member.asset.digest}`,
    name: message.role === "user" ? "User prompt.md" : "Agent reply.md",
    path: null,
    size: member.byte_len,
    ref: member.asset,
  };
  const bytes = new TextEncoder().encode(message.text);
  return {
    key: `message:${message.key}`,
    name: message.role === "user" ? "User prompt.md" : "Agent reply.md",
    path: null,
    size: bytes.byteLength,
    ref: { type: "transcript-message", bytes } satisfies MessagePayload,
  };
}

export const messageFileSource: FileSource = {
  async read(handle, signal) {
    signal.throwIfAborted();
    const payload = handle.ref as Partial<MessagePayload> | undefined;
    if (payload?.type !== "transcript-message" || !(payload.bytes instanceof Uint8Array)) {
      throw new Error("This message is no longer available");
    }
    return payload.bytes;
  },
};
