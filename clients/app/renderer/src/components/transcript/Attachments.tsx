import { useId, useLayoutEffect, useRef, useState } from "react";
import type { AttachmentFact } from "../../lib/transcript";
import { fmtBytes } from "../../lib/format";
import { attachmentAssetUrl } from "./attachment-file-source";
import "../../styles/attachments.css";

export function attachmentStatus(fact: AttachmentFact): string {
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
  const openRef = useRef<HTMLButtonElement>(null);
  const retryRef = useRef<HTMLButtonElement>(null);
  useLayoutEffect(() => {
    if (attempt > 0) (failed ? retryRef.current : openRef.current)?.focus({ preventScroll: true });
  }, [attempt, failed]);
  const url = fact.kind === "image" ? attachmentAssetUrl(fact) : null;
  const label = fact.ref?.label ?? `${fact.kind === "image" ? "Image" : "Attachment"} ${index + 1}`;
  const details = [fact.name, fact.caption, fact.byteLen === null ? null : fmtBytes(fact.byteLen)].filter(Boolean).join(" · ");
  return <figure data-attachment-id={fact.key}>
    {url === null || failed ? (
      <div className="tr-att__tile tr-att__tile--missing" tabIndex={-1}>
        <span>{failed ? "Could not load image" : fact.kind === "image" ? attachmentStatus(fact) : "Attachment preview unavailable"}</span>
        {failed && <button type="button" ref={retryRef} className="tr-att__retry" onClick={() => { setAttempt(value => value + 1); setFailed(false); }}>Retry</button>}
      </div>
    ) : (
      <button type="button" ref={openRef} className="tr-att__tile" aria-label={`Open ${label}${fact.name ? ` — ${fact.name}` : ""}`} onClick={() => onOpen(fact)}>
        <img key={attempt} src={attempt === 0 ? url : `${url}${url.includes("?") ? "&" : "?"}retry=${attempt}`}
          alt={fact.name ?? label} loading="lazy" decoding="async" onError={() => setFailed(true)} />
      </button>
    )}
    <figcaption><span className="tr-att__label">{label}</span>{details && <span title={details}>{details}</span>}</figcaption>
  </figure>;
}

export function Attachments({ facts, onOpenAttachment, reveal }: {
  readonly facts: readonly AttachmentFact[];
  readonly reveal?: { readonly key: string; readonly request: number } | null;
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const id = useId();
  const gallery = useRef<HTMLDivElement>(null);
  const handledReveal = useRef<typeof reveal>(undefined);
  useLayoutEffect(() => {
    if (!reveal || handledReveal.current === reveal) return;
    const index = facts.findIndex(fact => fact.key === reveal.key);
    if (index >= 4 && !expanded) { setExpanded(true); return; }
    const figure = [...(gallery.current?.querySelectorAll<HTMLElement>("[data-attachment-id]") ?? [])]
      .find(element => element.dataset.attachmentId === reveal.key);
    const target = figure?.querySelector<HTMLElement>(".tr-att__tile");
    target?.focus({ preventScroll: true });
    target?.scrollIntoView({ block: "nearest", behavior: "instant" });
    if (target) handledReveal.current = reveal;
  }, [reveal, facts, expanded]);
  if (facts.length === 0) return null;
  const visible = expanded ? facts : facts.slice(0, 4);
  return <div className="tr-attachments">
    <div className="tr-att" id={id} ref={gallery} data-single={facts.length === 1 || undefined}>
      {visible.map((fact, index) => <Tile key={`${fact.key}:${attachmentAssetUrl(fact) ?? "unavailable"}`} fact={fact} index={index} onOpen={onOpenAttachment} />)}
    </div>
    {facts.length > 4 && <button type="button" className="tr-att__more" aria-expanded={expanded} aria-controls={id}
      onClick={() => setExpanded(value => !value)}>{expanded ? "Show fewer images" : `Show all ${facts.length} images`}</button>}
  </div>;
}
