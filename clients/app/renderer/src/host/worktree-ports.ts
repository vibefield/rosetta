// The worktree engine's transferred ports, as the room's `HostPort.worktree`
// hands them over. Main posts them to the page (the preload forwards IPC as
// window messages); this module listens from load, because a transferred port
// is a one-owner capability that must survive an arrival before the room
// listens. What arrives early waits here, in order, for the one listener.
import type { WorktreeHostEvent } from "@rosetta/room";

type Listener = (event: WorktreeHostEvent) => void;

let listener: Listener | null = null;
const waiting: WorktreeHostEvent[] = [];

function deliver(event: WorktreeHostEvent): void {
  if (listener === null) waiting.push(event);
  else listener(event);
}

window.addEventListener("message", (event: MessageEvent) => {
  const data = event.data as
    | {
        readonly type?: string;
        readonly root?: string;
        readonly generation?: number;
        readonly state?: string;
        readonly message?: string;
      }
    | undefined;
  if (
    data?.type === "ah-worktree-ports" &&
    typeof data.root === "string" &&
    Number.isSafeInteger(data.generation) &&
    (data.generation ?? 0) > 0 &&
    event.ports.length === 2
  ) {
    deliver({
      type: "ports",
      root: data.root,
      generation: data.generation as number,
      treePort: event.ports[0]!,
      gitPort: event.ports[1]!,
    });
    return;
  }
  if (
    data?.type === "ah-worktree-status" &&
    typeof data.root === "string" &&
    Number.isSafeInteger(data.generation) &&
    (data.generation ?? 0) > 0 &&
    (data.state === "starting" || data.state === "error")
  ) {
    deliver({
      type: "status",
      root: data.root,
      generation: data.generation as number,
      state: data.state,
      ...(typeof data.message === "string" ? { message: data.message } : {}),
    });
  }
});

/** Listen to the engine; what arrived before is handed over first. */
export function worktreePorts(next: Listener): () => void {
  listener = next;
  for (const event of waiting.splice(0)) next(event);
  return () => {
    if (listener === next) listener = null;
  };
}
