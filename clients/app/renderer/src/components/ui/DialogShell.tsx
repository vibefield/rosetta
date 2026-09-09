// The one modal surface this room has, and the only place its mechanics live.
//
// Two things open over the room now — one file, and one commit — and they
// must be the same object to the reader: same material, same backdrop, same
// receding stage, same Escape. Sharing the chrome is how that stays true
// without either surface remembering to match the other.
//
// Radix owns what a modal must not get wrong: focus trap, focus restore,
// Escape, scroll lock, `aria-modal`, the portal.

import * as Dialog from "@radix-ui/react-dialog";
import { useRef } from "react";
import type { ReactNode } from "react";

export interface DialogShellProps {
  /** The accessible name. Rendered for screen readers only — the visible
   * band beside it is the sighted reader's version of the same fact. */
  readonly title: string;
  readonly onClose: () => void;
  /** Wider than the default, for a surface that holds two panes. */
  readonly wide?: boolean;
  /** Fill the application viewport while preserving the dialog's focus and
   * accessibility mechanics. */
  readonly fullscreen?: boolean;
  /** In fullscreen, the first Escape restores the dialog instead of closing
   * it. */
  readonly onExitFullscreen?: () => void;
  readonly children: ReactNode;
}

export function DialogShell({
  title,
  onClose,
  wide = false,
  fullscreen = false,
  onExitFullscreen,
  children,
}: DialogShellProps) {
  // These controlled dialogs have no Radix Trigger. Remember the actual
  // opener so closing a message returns to its preview without scrolling.
  const opener = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  return (
    <Dialog.Root open onOpenChange={(open) => !open && onClose()}>
      <Dialog.Portal>
        {/* `data-ah-fileview` is what the room's own stylesheet watches to
            let the stage recede behind an open dialog. */}
        <Dialog.Overlay className="ah-fileview__backdrop" data-ah-fileview />
        <Dialog.Content
          className="ah-fileview"
          data-wide={wide ? "" : undefined}
          data-fullscreen={fullscreen ? "" : undefined}
          aria-describedby={undefined}
          // Radix focuses the first tabbable child, which lights a focus ring
          // on a control nobody pressed. The dialog itself takes focus
          // instead — Escape and the focus trap still work, and Tab still
          // walks in.
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            (event.currentTarget as HTMLElement | null)?.focus();
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            if (opener.current?.isConnected) opener.current.focus({ preventScroll: true });
          }}
          onEscapeKeyDown={(event) => {
            if (!fullscreen) return;
            event.preventDefault();
            onExitFullscreen?.();
          }}
        >
          <Dialog.Title className="ah-visually-hidden">{title}</Dialog.Title>
          {children}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
