import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { AttachmentFact } from "../../lib/transcript";
import { fmtBytes } from "../../lib/format";
import { imageMediaTypeFromBytes } from "../../lib/image-media";
import { client } from "../../lib/rosetta";
import { attachmentAssetUrl } from "./attachment-file-source";
import "../../styles/attachments.css";

export function attachmentStatus(fact: AttachmentFact): string {
  if (fact.readPath) return "Image file is unavailable";
  const source = fact.ref?.source;
  if (source?.type === "unavailable") {
    switch (source.reason) {
      case "omitted": return "Image omitted from the recording";
      case "invalid_data": return "Recorded image data is invalid";
      case "unsupported_media": return "Image format is not supported";
      case "not_recorded": return "Image data was not recorded";
    }
  }
  return "Image data was not recorded";
}

function Tile({ fact, index, onOpen }: {
  readonly fact: AttachmentFact; readonly index: number;
  readonly onOpen: (fact: AttachmentFact) => void;
}) {
  const [failed, setFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [preview, setPreview] = useState<{ url: string; bytes: Uint8Array; mediaType: string | null } | null>(null);
  const figureRef = useRef<HTMLElement>(null);
  const openRef = useRef<HTMLButtonElement>(null);
  const retryRef = useRef<HTMLButtonElement>(null);
  const restoreFocus = useRef(false);
  const fail = () => {
    restoreFocus.current = Boolean(figureRef.current?.contains(document.activeElement));
    setFailed(true);
  };
  const address = fact.readPath?.read;
  const mediaType = fact.mediaType;
  const sessionId = address?.sessionId, timelineId = address?.timelineId, targetIndex = address?.targetIndex;
  useEffect(() => {
    if (!sessionId || !timelineId || targetIndex === undefined || !figureRef.current) return;
    const controller = new AbortController();
    let objectUrl: string | null = null;
    const observer = new IntersectionObserver(entries => {
      if (!entries.some(entry => entry.isIntersecting)) return;
      observer.disconnect();
      void client.readFileTarget(sessionId, timelineId, targetIndex, controller.signal)
        .then(bytes => {
          if (controller.signal.aborted) return;
          const type = mediaType ?? imageMediaTypeFromBytes(bytes);
          objectUrl = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: type ?? "application/octet-stream" }));
          restoreFocus.current = Boolean(figureRef.current?.contains(document.activeElement));
          setPreview({ url: objectUrl, bytes, mediaType: type });
        })
        .catch(() => { if (!controller.signal.aborted) {
          restoreFocus.current = Boolean(figureRef.current?.contains(document.activeElement));
          setFailed(true);
        } });
    }, { rootMargin: "160px" });
    observer.observe(figureRef.current);
    return () => { controller.abort(); observer.disconnect(); if (objectUrl) URL.revokeObjectURL(objectUrl); };
  }, [sessionId, timelineId, targetIndex, mediaType, attempt]);
  useLayoutEffect(() => {
    if (!restoreFocus.current) return;
    const target = failed ? retryRef.current : openRef.current ?? figureRef.current?.querySelector<HTMLElement>(".tr-att__tile");
    target?.focus({ preventScroll: true });
    restoreFocus.current = false;
  }, [attempt, failed, preview]);
  const assetUrl = fact.kind === "image" ? attachmentAssetUrl(fact) : null;
  const url = preview?.url ?? assetUrl;
  const loading = Boolean(address) && url === null && !failed;
  const label = fact.ref?.label ?? `${fact.kind === "image" ? "Image" : "Attachment"} ${index + 1}`;
  const details = [label, fact.name, fact.caption, fact.byteLen === null ? null : fmtBytes(fact.byteLen)].filter(Boolean).join(" · ");
  return <figure ref={figureRef} data-attachment-id={fact.key} title={details}>
    {url === null || failed ? (
      <div className={`tr-att__tile tr-att__tile--${loading ? "loading" : "missing"}`} tabIndex={-1} aria-label={details} aria-busy={loading}>
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden="true">
          <rect x="3" y="3" width="18" height="18" rx="4" /><circle cx="8" cy="8" r="1.5" />
          <path d="m3 17 5-5 4 4 3-3 6 6" />
        </svg>
        <span>{loading ? "Loading image…" : failed ? "Could not load image" : fact.kind === "image" ? attachmentStatus(fact) : "Attachment preview unavailable"}</span>
        {failed && <button type="button" ref={retryRef} className="tr-att__retry" onClick={() => { restoreFocus.current = true; setPreview(null); setAttempt(value => value + 1); setFailed(false); }}>Retry</button>}
      </div>
    ) : (
      <button type="button" ref={openRef} className="tr-att__tile" aria-label={`Open ${label}${fact.name ? ` — ${fact.name}` : ""}`}
        aria-haspopup="dialog" onClick={() => onOpen(preview ? { ...fact, mediaType: preview.mediaType, readBytes: preview.bytes } : fact)}>
        <img key={attempt} src={preview || attempt === 0 ? url : `${url}${url.includes("?") ? "&" : "?"}retry=${attempt}`}
          alt={fact.name ?? label} loading="lazy" decoding="async" onError={fail} />
      </button>
    )}
  </figure>;
}

/** Scroll just this rail: image navigation must never move the room or transcript. */
function revealTile(rail: HTMLElement, tile: HTMLElement) {
  const bounds = rail.getBoundingClientRect();
  const target = tile.getBoundingClientRect();
  if (target.left < bounds.left + 4) rail.scrollLeft += target.left - bounds.left - 4;
  else if (target.right > bounds.right - 4) rail.scrollLeft += target.right - bounds.right + 4;
}

export function Attachments({ facts, onOpenAttachment, reveal }: {
  readonly facts: readonly AttachmentFact[];
  readonly reveal?: { readonly key: string; readonly request: number } | null;
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
}) {
  const id = useId();
  const gallery = useRef<HTMLDivElement>(null);
  const handledReveal = useRef<typeof reveal>(undefined);
  const [edges, setEdges] = useState({ before: false, after: false });
  useLayoutEffect(() => {
    const rail = gallery.current;
    if (!rail) return;
    const update = () => {
      const before = rail.scrollLeft > 2;
      const after = rail.scrollLeft + rail.clientWidth < rail.scrollWidth - 2;
      setEdges(old => old.before === before && old.after === after ? old : { before, after });
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(rail);
    rail.addEventListener("scroll", update, { passive: true });
    return () => { observer.disconnect(); rail.removeEventListener("scroll", update); };
  }, [facts.length]);
  useLayoutEffect(() => {
    if (!reveal || handledReveal.current === reveal || !gallery.current) return;
    const figure = [...gallery.current.querySelectorAll<HTMLElement>("[data-attachment-id]")]
      .find(element => element.dataset.attachmentId === reveal.key);
    const target = figure?.querySelector<HTMLElement>(".tr-att__tile");
    if (!target) return;
    target.focus({ preventScroll: true });
    revealTile(gallery.current, target);
    // A missing-image mention may sit far above its strip in the full message.
    // Keep vertical scrolling inside the nearest reading surface.
    const scroller = gallery.current.closest<HTMLElement>(".ah-fileview__scroll, .tr-scroll");
    if (scroller) {
      const bounds = scroller.getBoundingClientRect();
      const rect = target.getBoundingClientRect();
      if (rect.top < bounds.top) scroller.scrollTop += rect.top - bounds.top - 12;
      else if (rect.bottom > bounds.bottom) scroller.scrollTop += rect.bottom - bounds.bottom + 12;
    }
    handledReveal.current = reveal;
  }, [reveal, facts]);
  if (facts.length === 0) return null;
  const page = (direction: number) => gallery.current?.scrollBy({ left: direction * gallery.current.clientWidth * 0.8,
    behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth" });
  return <div className="tr-attachments" role="group" aria-label={`${facts.length} ${facts.length === 1 ? "attachment" : "attachments"}`}>
    <div className="tr-att" id={id} ref={gallery} onFocusCapture={event => {
      const tile = event.target.closest<HTMLElement>(".tr-att__tile");
      if (tile && gallery.current) revealTile(gallery.current, tile);
    }} onKeyDown={event => {
      if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key) || !gallery.current) return;
      const tiles = [...gallery.current.querySelectorAll<HTMLElement>(".tr-att__tile")];
      const index = tiles.findIndex(tile => tile.contains(event.target as Node));
      if (index < 0) return;
      event.preventDefault();
      const next = event.key === "Home" ? 0 : event.key === "End" ? tiles.length - 1 : Math.max(0, Math.min(tiles.length - 1, index + (event.key === "ArrowRight" ? 1 : -1)));
      tiles[next]?.focus({ preventScroll: true });
    }}>
      {facts.map((fact, index) => <Tile key={`${fact.key}:${attachmentAssetUrl(fact) ?? fact.readPath?.label ?? "reference"}`} fact={fact} index={index} onOpen={onOpenAttachment} />)}
    </div>
    {(edges.before || edges.after) && <button className="tr-att__nav tr-att__nav--previous" type="button" aria-label="Previous images" aria-disabled={!edges.before} data-edge={!edges.before || undefined} aria-controls={id} onClick={() => page(-1)}>‹</button>}
    {(edges.before || edges.after) && <button className="tr-att__nav tr-att__nav--next" type="button" aria-label="Next images" aria-disabled={!edges.after} data-edge={!edges.after || undefined} aria-controls={id} onClick={() => page(1)}>›</button>}
  </div>;
}
