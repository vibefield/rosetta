import { useEffect, useMemo, useState } from "react";
import type { ViewBodyProps } from "../types";
import { imageRequestUrl } from "../image-resource";

export const IMAGE_MODES = [
  { id: "fit", label: "Fit" },
  { id: "actual", label: "1:1" },
] as const;

export function ImageView({ file, mode, onRetry }: ViewBodyProps) {
  const [natural, setNatural] = useState<{ width: number; height: number } | null>(null);
  const [failedUrl, setFailedUrl] = useState<string | null>(null);
  const [loadedUrl, setLoadedUrl] = useState<string | null>(null);
  // Byte-only sources retain the generic viewer seam. Transcript images use
  // shared HTTP resources and allocate no Blob.
  const blob = useMemo(() => file.image ? null : URL.createObjectURL(
    new Blob([file.bytes as BlobPart], { type: file.kind.mime ?? "application/octet-stream" })), [file]);
  useEffect(() => () => { if (blob) URL.revokeObjectURL(blob); }, [blob]);
  const url = file.image
    ? imageRequestUrl(mode === "actual" ? file.image.full : file.image.preview, file.imageAttempt)
    : blob!;
  const dimensions = file.image?.width && file.image.height ? file.image : natural;

  if (failedUrl === url) return <div className="ah-fileview__state">
    <b>Could not load this image</b>
    <span>The image could not be read or decoded.</span>
    {onRetry && <button className="ah-fileview__retry" type="button" onClick={onRetry}>Retry</button>}
  </div>;

  return <div className="ah-image" data-mode={mode} aria-busy={loadedUrl !== url}>
    {loadedUrl !== url && <span className="ah-image__loading" role="status">Loading image…</span>}
    <div className="ah-image__frame ah-fileview__scroll">
      <img key={url} src={url} alt={file.handle.name} decoding="async" onLoad={event => {
        setLoadedUrl(url);
        setNatural({ width: event.currentTarget.naturalWidth, height: event.currentTarget.naturalHeight });
      }} onError={() => setFailedUrl(url)} />
    </div>
    {dimensions && <span className="ah-image__facts tnum">{dimensions.width} × {dimensions.height}</span>}
  </div>;
}
