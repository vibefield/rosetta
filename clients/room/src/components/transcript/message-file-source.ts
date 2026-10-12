import type { CanonicalText } from "@vibefield/rosetta-protocol";
import type { AttachmentFact } from "../../lib/transcript";
import type { FileHandle, FileSource } from "../file-viewer/types";

export interface FullMessage {
  readonly key: string;
  readonly role: "user" | "agent";
  /** Generated context can name its reading view without changing authorship. */
  readonly title?: string | undefined;
  readonly text: string;
  readonly recordedText?: CanonicalText | null | undefined;
  readonly attachments?: readonly AttachmentFact[] | undefined;
}

interface MessagePayload {
  readonly type: "transcript-message";
  readonly bytes: Uint8Array;
}

/** A full canonical member, or a snapshot of complete inline text. */
export function handleForMessage(message: FullMessage): FileHandle {
  const name = `${message.title ?? (message.role === "user" ? "User prompt" : "Agent reply")}.md`;
  const member = message.recordedText?.type === "member" ? message.recordedText : null;
  if (member !== null) return {
    key: `message:${message.key}:${member.asset.digest}`,
    name,
    path: null,
    size: member.byte_len,
    ref: member.asset,
  };
  const bytes = new TextEncoder().encode(message.text);
  return {
    key: `message:${message.key}`,
    name,
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
