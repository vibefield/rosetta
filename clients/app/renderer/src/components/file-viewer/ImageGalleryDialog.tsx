import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { FileViewerDialog } from "./FileViewerDialog";
import type { FileViewerDialogProps } from "./FileViewerDialog";
import type { FileHandle, FileSource } from "./types";
import { createGallerySource } from "./gallery-source";
import { useFileContent } from "./use-file-content";
import { useImageUrl, useImageVisibility } from "./image-resource";
import "../../styles/image-gallery.css";

export interface GalleryImage {
  readonly key: string;
  readonly label: string;
  /** Unrecorded/reference-only images keep their place without an invented read. */
  readonly handle: FileHandle | null;
}

function Thumbnail({ image, source, selected, index, revision, onSelect }: {
  image: GalleryImage; source: FileSource; selected: boolean; index: number; revision: number; onSelect: () => void;
}) {
  const ref = useRef<HTMLButtonElement>(null);
  const visible = useImageVisibility(ref, true);
  const [bytesUrl, setBytesUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const content = useFileContent(visible || selected ? image.handle : null, source, false, revision, true);
  const addressed = useImageUrl(content.status === "ready" ? content.file.image : null, "rail", revision);
  useEffect(() => {
    if (content.status !== "ready") { setBytesUrl(null); return; }
    setFailed(false);
    if (content.file.image) { setBytesUrl(null); return; }
    const objectUrl = URL.createObjectURL(new Blob([content.file.bytes as BlobPart], { type: content.file.kind.mime ?? "application/octet-stream" }));
    setBytesUrl(objectUrl);
    return () => URL.revokeObjectURL(objectUrl);
  }, [content, revision]);
  const url = content.status === "ready" && content.file.image ? addressed.url : bytesUrl;
  const label = `${image.label}${image.handle ? ` — ${image.handle.name}` : " — Image unavailable"}`;
  return <button ref={ref} className="ah-image-gallery__thumb" type="button"
    data-image-key={image.key} aria-label={`View ${label}`} title={label}
    disabled={image.handle === null} aria-current={selected ? "true" : undefined}
    tabIndex={selected ? 0 : -1} onClick={onSelect}>
    {(visible || selected) && url && !failed ? <img src={url} alt="" decoding="async" onError={() => setFailed(true)} />
      : <span aria-hidden="true">{image.handle === null ? "▧" : index + 1}</span>}
  </button>;
}

export interface ImageGalleryDialogProps extends Omit<FileViewerDialogProps, "handle" | "gallery" | "asDiff" | "renderMarkdown"> {
  readonly images: readonly GalleryImage[];
  readonly initialKey: string;
}

export function ImageGalleryDialog({ images, initialKey, source, ...props }: ImageGalleryDialogProps) {
  const id = useId();
  const [selectedKey, setSelectedKey] = useState(initialKey);
  const [revision, setRevision] = useState(0);
  const shared = useMemo(() => createGallerySource(source), [source]);
  useEffect(() => () => shared.dispose(), [shared]);
  const rail = useRef<HTMLDivElement>(null);
  const selected = images.findIndex(image => image.key === selectedKey);
  const current = images[selected];
  const available = images.flatMap((image, index) => image.handle === null ? [] : [index]);
  const position = available.indexOf(selected);
  const previous = available[position - 1] ?? -1;
  const next = available[position + 1] ?? -1;
  useLayoutEffect(() => {
    const node = rail.current;
    const thumb = node?.querySelector<HTMLElement>('[aria-current="true"]');
    if (!node || !thumb) return;
    const reveal = () => {
      const bounds = node.getBoundingClientRect(), target = thumb.getBoundingClientRect();
      if (target.left < bounds.left + 4) node.scrollLeft += target.left - bounds.left - 4;
      else if (target.right > bounds.right - 4) node.scrollLeft += target.right - bounds.right + 4;
    };
    reveal();
    const observer = new ResizeObserver(reveal);
    observer.observe(node);
    return () => observer.disconnect();
  }, [selectedKey]);
  if (!current?.handle) return null;
  const select = (index: number, focus = false) => {
    const image = images[index];
    if (!image?.handle) return;
    setSelectedKey(image.key);
    if (focus) rail.current?.querySelectorAll<HTMLButtonElement>("button")[index]?.focus({ preventScroll: true });
  };
  const navigate = (event: KeyboardEvent<HTMLDivElement>) => {
    if (images.length <= 1) return;
    if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    if (!(event.target instanceof Element) || event.target.closest('input, textarea, select, [contenteditable="true"], .ah-code-shell')) return;
    const index = event.key === "ArrowLeft" ? previous : event.key === "ArrowRight" ? next
      : event.key === "Home" ? available[0] ?? -1
      : event.key === "End" ? available.at(-1) ?? -1 : null;
    if (index === null) return;
    event.preventDefault(); event.stopPropagation();
    select(index, Boolean(event.target.closest(".ah-image-gallery__rail")));
  };
  return <FileViewerDialog {...props} handle={current.handle} source={shared.source}
    gallery={{ key: id, onKeyDown: navigate, onRetry: () => {
      shared.invalidate(current.handle!.key); setRevision(value => value + 1);
    },
      previous: previous >= 0 ? () => select(previous) : null,
      next: next >= 0 ? () => select(next) : null,
      footer: images.length > 1 ? <footer className="ah-image-gallery">
        <div className="ah-image-gallery__caption"><span>Images in this row</span>
          <span role="status" aria-live="polite" aria-atomic="true">{selected + 1} / {images.length}</span></div>
        <div className="ah-image-gallery__rail" ref={rail} role="group" aria-label="Images in this row">
          {images.map((image, index) => <Thumbnail key={image.key} image={image} source={shared.source}
            index={index} revision={revision} selected={index === selected} onSelect={() => select(index)} />)}
        </div>
      </footer> : null,
    }} />;
}
