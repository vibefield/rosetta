// A file nothing here can read as a document. The honest answer is not an
// error and not an empty frame: it is the file's first bytes, which is the
// only thing anyone can truthfully say about it.

import { useMemo } from "react";
import type { ViewBodyProps } from "../types";

/** How much of the file the dump shows. A hex view is for identifying a
 * file — a magic number, a header, an encoding — not for reading it. */
const DUMP_BYTES = 4096;
const COLUMNS = 16;

const HEX = Array.from({ length: 256 }, (_, byte) => byte.toString(16).padStart(2, "0"));

interface Row {
  readonly offset: string;
  readonly hex: string;
  readonly text: string;
}

function dump(bytes: Uint8Array): readonly Row[] {
  const rows: Row[] = [];
  const end = Math.min(bytes.length, DUMP_BYTES);
  for (let at = 0; at < end; at += COLUMNS) {
    let hex = "";
    let text = "";
    for (let column = 0; column < COLUMNS; column += 1) {
      const byte = at + column < end ? bytes[at + column] : null;
      hex += byte === null ? "   " : `${HEX[byte!]} `;
      // Printable ASCII only; everything else is a dot, the way every hex
      // dump since 1970 has drawn it.
      text += byte === null ? " " : byte! >= 0x20 && byte! < 0x7f ? String.fromCharCode(byte!) : ".";
    }
    rows.push({ offset: at.toString(16).padStart(8, "0"), hex, text });
  }
  return rows;
}

export function BinaryView({ file }: ViewBodyProps) {
  const rows = useMemo(() => dump(file.bytes), [file]);
  const shown = Math.min(file.bytes.length, DUMP_BYTES);

  return (
    <div className="ah-fileview__scroll ah-hex">
      <p className="ah-hex__note">
        Nothing here reads this format.{" "}
        {shown < file.handle.size
          ? `These are the first ${shown.toLocaleString()} bytes of ${file.handle.size.toLocaleString()}.`
          : `All ${shown.toLocaleString()} bytes are here.`}
      </p>
      {rows.map((row) => (
        <div className="ah-hex__row" key={row.offset}>
          <span className="ah-hex__offset">{row.offset}</span>
          <span className="ah-hex__bytes">{row.hex}</span>
          <span className="ah-hex__text">{row.text}</span>
        </div>
      ))}
    </div>
  );
}
