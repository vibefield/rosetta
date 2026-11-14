// A video an agent sent, in the attachment rail. Before the reader asks, it is
// a tile with a play glyph, the file's name and its size: no poster, no
// thumbnail, no request. A press plays it in place. A video that was sent but
// not captured keeps the same tile shape and says so; it is not a button.
// A video never opens the image viewer and never asks an image rendition.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { AttachmentFact } from "../../lib/transcript";
import { fmtBytes } from "../../lib/format";
import { useAttachmentReader } from "./attachment-file-source";
import { IDLE_VIDEO, createVideoSource } from "./video-source";
import type { VideoSource, VideoState } from "./video-source";

const WIDEST = 21 / 9;

/** Width over height for the tile at the rail's height: the video's own
 * shape, 16:9 when it was not measured. Never narrower than the square image
 * tile beside it, nor wider than 21:9; the video letterboxes inside. */
export function videoAspect(width: number | null, height: number | null): number {
  if (width === null || height === null || !(width > 0) || !(height > 0)) return 16 / 9;
  return Math.min(WIDEST, Math.max(1, width / height));
}

/** Why a video cannot be played here. */
export function videoStatus(fact: AttachmentFact): string {
  const source = fact.ref?.source;
  if (source?.type === "reference") return "Video not captured";
  // Recorded, but not a member this room can address: the host offers no
  // address for it, or it is not an MP4 or QuickTime member under its digest.
  if (source?.type === "asset") return "Video is not available here";
  if (source?.type === "unavailable") {
    switch (source.reason) {
      case "omitted": return "Video omitted from the recording";
      case "invalid_data": return "Recorded video data is invalid";
      case "unsupported_media": return "Video format is not supported";
      case "not_recorded": return "Video data was not recorded";
    }
  }
  return "Video data was not recorded";
}

function PlayGlyph() {
  return <svg width="28" height="28" viewBox="0 0 28 28" fill="none" aria-hidden="true">
    <circle cx="14" cy="14" r="12.5" stroke="currentColor" strokeWidth="1.3" />
    <path d="M11.5 9.2v9.6l8-4.8z" fill="currentColor" />
  </svg>;
}

function FilmGlyph() {
  return <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden="true">
    <rect x="3" y="5" width="18" height="14" rx="3" /><path d="m10 9.5 4.5 2.5-4.5 2.5z" />
  </svg>;
}

export interface VideoFaceProps {
  readonly fact: AttachmentFact;
  /** The video's place among the rail's videos, for an unlabelled one's name. */
  readonly index: number;
  /** False: sent but not captured, or a member this host does not offer. */
  readonly playable: boolean;
  readonly state: VideoState;
  readonly onPlay?: (() => void) | undefined;
  readonly onElementError?: ((url: string) => void) | undefined;
  /** The face's one focus target: the play button, the video, or Retry. */
  readonly focusRef?: ((element: HTMLElement | null) => void) | undefined;
  readonly figureRef?: ((element: HTMLElement | null) => void) | undefined;
}

/** One state of the tile, drawn from its props alone. */
export function VideoFace({ fact, index, playable, state, onPlay, onElementError, focusRef, figureRef }: VideoFaceProps) {
  const label = fact.ref?.label ?? `Video ${index + 1}`;
  const name = fact.name?.trim() || label;
  const size = fact.byteLen === null ? null : fmtBytes(fact.byteLen);
  const facts = [name, size].filter(Boolean).join(" · ");
  const details = [label, fact.name, fact.caption, size].filter(Boolean).join(" · ");
  const style = { "--tr-video-aspect": videoAspect(fact.width, fact.height).toFixed(4) } as CSSProperties;
  const playing = playable && state.phase === "ready";
  return <figure ref={figureRef} className="tr-att__video" data-attachment-id={fact.key} data-video={playable ? state.phase : "uncaptured"}
    title={playing ? undefined : details} style={style}>
    {!playable ? (
      <div className="tr-att__tile tr-att__tile--video tr-att__tile--missing" tabIndex={-1} aria-label={`${videoStatus(fact)} — ${details}`}>
        <FilmGlyph />
        <span>{videoStatus(fact)}</span>
        <span className="tr-att__facts">{facts}</span>
      </div>
    ) : state.phase === "failed" ? (
      <div className="tr-att__tile tr-att__tile--video tr-att__tile--missing" tabIndex={-1} aria-label={`Could not play video — ${details}`}>
        <FilmGlyph />
        <span>Could not play video</span>
        <button type="button" ref={focusRef} className="tr-att__retry" aria-label={`Retry video — ${name}`} onClick={onPlay}>Retry</button>
      </div>
    ) : state.phase === "ready" ? (
      <div className="tr-att__tile tr-att__tile--video tr-att__tile--playing" tabIndex={-1}>
        <video key={state.url} ref={focusRef} src={state.url} controls autoPlay playsInline preload="auto"
          aria-label={name} onError={() => onElementError?.(state.url)} />
      </div>
    ) : (
      <button type="button" ref={focusRef} aria-busy={state.phase === "loading"}
        className={state.phase === "loading" ? "tr-att__tile tr-att__tile--video tr-att__tile--loading" : "tr-att__tile tr-att__tile--video"}
        aria-label={state.phase === "loading" ? `Loading video — ${name}` : `Play video — ${facts}`} onClick={onPlay}>
        <PlayGlyph />
        <span className="tr-att__name">{name}</span>
        <span className="tr-att__facts">{state.phase === "loading" ? "Loading video…" : size ?? "Video"}</span>
      </button>
    )}
  </figure>;
}

/** The tile over its load: nothing is asked until the reader presses play. */
export function VideoTile({ fact, index }: { readonly fact: AttachmentFact; readonly index: number }) {
  const attachments = useAttachmentReader();
  const asset = attachments.videoAsset(fact);
  const digest = asset?.digest ?? null;
  const [state, setState] = useState<VideoState>(IDLE_VIDEO);
  const source = useRef<VideoSource | null>(null);
  const figure = useRef<HTMLElement | null>(null);
  const target = useRef<HTMLElement | null>(null);
  const restoreFocus = useRef(false);
  // Leaving the rail, or the member or its reader changing, ends the load:
  // the read is aborted and an object URL made for it is revoked.
  useEffect(() => () => {
    source.current?.dispose();
    source.current = null;
    setState(IDLE_VIDEO);
  }, [digest, attachments]);
  // A press swaps the button for the video (or for Retry). Keep the reader's
  // focus on the tile through each swap, unless they have moved elsewhere.
  useLayoutEffect(() => {
    if (!restoreFocus.current) return;
    const active = document.activeElement;
    if (active !== null && active !== document.body && !figure.current?.contains(active)) {
      restoreFocus.current = false;
      return;
    }
    target.current?.focus({ preventScroll: true });
  }, [state]);
  const play = () => {
    if (asset === null) return;
    restoreFocus.current = true;
    source.current ??= createVideoSource(asset, {
      mediaAssetUrl: attachments.mediaAssetUrl,
      readAsset: attachments.readAsset,
      createObjectURL: (bytes, mediaType) => URL.createObjectURL(new Blob([bytes as BlobPart], { type: mediaType })),
      revokeObjectURL: (url) => URL.revokeObjectURL(url),
      onChange: setState,
    });
    source.current.play();
  };
  return <VideoFace fact={fact} index={index} playable={asset !== null} state={state} onPlay={play}
    onElementError={(url) => source.current?.elementFailed(url)}
    focusRef={(element) => { target.current = element; }} figureRef={(element) => { figure.current = element; }} />;
}
