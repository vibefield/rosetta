// The one thing this room asks before doing.
//
// Reading history needs no confirmation and never gets one. Discarding a
// file does: it destroys work that exists nowhere else, and git keeps no
// reflog for a working-tree change. So the act names what it will destroy
// and waits — which is the difference between a tool and a trap.
//
// Same material as the file viewer, one size down: this is the only other
// thing in the room that covers it.

import * as Dialog from "@radix-ui/react-dialog";
import type { ReactNode } from "react";
import { useRoomOverlay, useRoomPortal } from "../../room-context";

export interface ConfirmDialogProps {
  /** `null` when nothing is being asked. */
  readonly question: {
    readonly title: string;
    readonly body: ReactNode;
    /** What the confirming button says — a verb, not "OK". */
    readonly confirm: string;
    readonly destructive?: boolean;
  } | null;
  readonly onConfirm: () => void;
  readonly onCancel: () => void;
}

export function ConfirmDialog({ question, onConfirm, onCancel }: ConfirmDialogProps) {
  useRoomOverlay(question !== null);
  const container = useRoomPortal();
  if (question === null) return null;
  return (
    <Dialog.Root open onOpenChange={(open) => !open && onCancel()}>
      <Dialog.Portal container={container}>
        <Dialog.Overlay className="ah-layer ah-fileview__backdrop" data-ah-fileview />
        <Dialog.Content className="ah-layer ah-confirm" aria-describedby={undefined}>
          <Dialog.Title className="ah-confirm__title">{question.title}</Dialog.Title>
          <div className="ah-confirm__body">{question.body}</div>
          <div className="ah-confirm__acts">
            <button type="button" className="ah-confirm__cancel" onClick={onCancel}>
              Cancel
            </button>
            <button
              type="button"
              className="ah-confirm__go"
              data-destructive={question.destructive === true ? "" : undefined}
              // The destructive act does NOT take focus on open: a reader who
              // opens this and hits Return should cancel, not destroy.
              onClick={onConfirm}
            >
              {question.confirm}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
