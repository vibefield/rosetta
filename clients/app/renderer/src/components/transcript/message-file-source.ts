import type { FileHandle, FileSource } from "../file-viewer/types.ts";

export interface FullMessage {
  readonly key: string;
  readonly role: "user" | "agent";
  readonly text: string;
}

interface MessagePayload {
  readonly type: "transcript-message";
  readonly bytes: Uint8Array;
}

/** A snapshot of exactly what the reader opened, with no filesystem read. */
export function handleForMessage(message: FullMessage): FileHandle {
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
