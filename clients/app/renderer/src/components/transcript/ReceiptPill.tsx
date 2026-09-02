// §08 — the receipt. "The viewport did not move; the pill is the receipt and
// fades." The foot's new-messages pill, mirrored to the head.
//
// MOUNT: absolutely positioned, so its host must be the positioned frame that
// holds the scroller — the transcript column already is. Pass `top` so it
// lands BENEATH a pinned day pill, never over it; the record's live frame uses
// 120px, and the bare rule's own default is 10px.
import type { CSSProperties } from "react";
import "../../styles/transcript-head.css";

export interface ReceiptPillProps {
  readonly receipt: { readonly count: number; readonly seq: number } | null;
  /** Pixels from the frame's top. Clear the pinned day pill. */
  readonly top?: number;
}

export function ReceiptPill({ receipt, top = 120 }: ReceiptPillProps) {
  if (receipt === null || receipt.count <= 0) return null;
  return (
    // Keyed on the landing so a second receipt inside the first one's dwell
    // remounts, and the pop plays again rather than sliding a number.
    <div
      key={receipt.seq}
      className="tr-headpill"
      style={{ "--tr-headpill-top": `${top}px` } as CSSProperties}
    >
      <i />
      {receipt.count} earlier loaded
    </div>
  );
}
