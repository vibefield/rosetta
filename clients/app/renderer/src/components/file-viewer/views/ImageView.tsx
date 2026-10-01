// An image, from bytes the renderer already holds.
//
// A blob URL rather than a `file://` src: the room has no filesystem access
// and should not gain one to draw a thumbnail, and a blob is revoked the
// moment the dialog closes, so an image cannot outlive the reading of it.

import { useEffect, useMemo, useState } from "react";
import type { ViewBodyProps } from "../types";

export const IMAGE_MODES = [
  { id: "fit", label: "Fit" },
  { id: "actual", label: "1:1" },
] as const;

interface Natural {
  readonly width: number;
  readonly height: number;
}

export function ImageView({ file, mode, onRetry }: ViewBodyProps) {
  const [natural, setNatural] = useState<Natural | null>(null);
  const [failed, setFailed] = useState(false);

  const url = useMemo(() => {
    const type = file.kind.mime ?? "application/octet-stream";
    // `bytes` is a view; Blob copies it, so the URL does not pin the read
    // buffer for the life of the dialog.
    return URL.createObjectURL(new Blob([file.bytes as BlobPart], { type }));
  }, [file]);

  useEffect(() => () => URL.revokeObjectURL(url), [url]);

  if (failed) {
    return (
      <div className="ah-fileview__state">
        <b>This is not an image the window can draw</b>
        <span>
          The bytes are there — {file.bytes.byteLength.toLocaleString()} of them — but the decoder
          refused them. A wrong extension is the usual reason.
        </span>
        {onRetry && <button className="ah-fileview__retry" type="button" onClick={onRetry}>Retry</button>}
      </div>
    );
  }

  return (
    // The frame scrolls and the facts sit OVER it, rather than beside the
    // image: a flex sibling gets laid out next to the picture and, on a wide
    // one, wraps into the middle of it.
    <div className="ah-image" data-mode={mode}>
      <div className="ah-image__frame ah-fileview__scroll">
        <img
          src={url}
          alt={file.handle.name}
          onLoad={(event) =>
            setNatural({
              width: event.currentTarget.naturalWidth,
              height: event.currentTarget.naturalHeight,
            })
          }
          onError={() => setFailed(true)}
        />
      </div>
      {natural !== null && (
        <span className="ah-image__facts tnum">
          {natural.width} × {natural.height}
        </span>
      )}
    </div>
  );
}
