import { useEffect, useId, useMemo, useRef } from "react";
import type { ReactNode } from "react";
import type { TimelineEntry } from "@rosetta/protocol";
import { clock } from "../../lib/format";
import type { ActivityItem } from "./activity-plan";
import { activityRowKey, summarizeActivity } from "./activity-plan";
import { useGroupDisclosure, useRowDisclosure } from "./disclosure-state";
import { useFoldPreference } from "./fold-preference";
import { FamilyGlyph, LineChevron } from "./glyphs";
import { MessagePreview } from "./MessagePreview";
import type { AttachmentFact, Segment } from "../../lib/transcript";
import { timelineImages } from "../../lib/transcript/images";
import { ActivityImagesContext, Attachments, useAttachmentActions } from "./Attachments";
import { scrollRowTo } from "./scroll-to-row";
import type { EarlierState } from "../../lib/transcript/head-state";
import "../../styles/transcript-activity.css";

export function revealActivityTarget(element: HTMLElement): void {
  if (element.getClientRects().length === 0) return;
  const scroller = element.closest<HTMLElement>(".tr-scroll");
  if (scroller !== null) scrollRowTo(scroller, element, "center");
  const target = element.matches("button, summary, [tabindex]") ? element
    : element.querySelector<HTMLElement>("summary, button, [tabindex]");
  target?.focus({ preventScroll: true });
}

export function ActivityGroup({ items, rows, partial, forceOpen, highlightedId, alive, onOpenAttachment, onReceipt,
  earlier, onLoadEarlier, teamLoading, teamError, onRetryTeam, children }: {
  readonly items: readonly ActivityItem[];
  readonly rows: readonly TimelineEntry[];
  readonly partial: boolean;
  readonly earlier: EarlierState;
  readonly onLoadEarlier: () => void;
  readonly teamLoading: boolean;
  readonly teamError: string | null;
  readonly onRetryTeam: () => void;
  readonly forceOpen: boolean;
  readonly highlightedId: string | null;
  readonly alive: (row: TimelineEntry) => boolean;
  readonly onOpenAttachment: (fact: AttachmentFact, row?: readonly AttachmentFact[]) => void;
  readonly onReceipt: (text: string) => void;
  readonly children: ReactNode;
}) {
  const preference = useFoldPreference();
  const keys = rows.map(activityRowKey);
  const [override, setOverride] = useGroupDisclosure(keys, preference.revision);
  const expanded = override ?? (!preference.folded || forceOpen || highlightedId !== null);
  const listId = useId();
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (forceOpen || highlightedId !== null) setOverride(true);
    // Arrival patches change keys, but must not reopen a deliberately shut run.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [forceOpen, highlightedId]);
  useEffect(() => {
    if (highlightedId === null || !expanded) return;
    const frame = requestAnimationFrame(() => {
      const target = root.current?.querySelector<HTMLElement>(`[id][data-timeline-id="${CSS.escape(highlightedId)}"]`);
      if (target !== null && target !== undefined) revealActivityTarget(target);
    });
    return () => cancelAnimationFrame(frame);
  }, [expanded, highlightedId]);
  const summary = useMemo(() => summarizeActivity(items), [items]);
  const images = useMemo(() => timelineImages(rows), [rows]);
  const { open: openImage, reveal, onPreview } = useAttachmentActions(onOpenAttachment, onReceipt, images);
  const running = summary.runningRows.some(alive);
  const text = summary.nouns.map(noun => [noun.text, noun.names.join(", "), noun.more > 0 ? `… ${noun.more} more` : ""].filter(Boolean).join(" ")).join(" · ");
  const range = summary.firstAt === null ? null : summary.lastAt === null || summary.firstAt === summary.lastAt
    ? clock(summary.firstAt) : `${clock(summary.firstAt)}–${clock(summary.lastAt)}`;
  const label = [partial ? "More activity to load" : null, teamLoading ? "Loading team activity" : null, text, summary.failed > 0 ? `${summary.failed} failed` : null,
    summary.interrupted > 0 ? `${summary.interrupted} interrupted` : null,
    summary.cancelled > 0 ? `${summary.cancelled} cancelled` : null, running ? "Running" : null, range].filter(Boolean).join(" · ");
  return <div className="tr-seg tr-activity" ref={root} data-partial={partial || undefined}>
    <button type="button" className="tr-line tr-fold tr-activity__head" aria-expanded={expanded}
      aria-controls={listId} aria-label={label} title={label} onClick={() => {
        if (!expanded && partial) onLoadEarlier();
        setOverride(!expanded);
      }}>
      <span className="tr-line__g chev"><LineChevron /></span>
      <span className="tr-line__v">
        {partial && <span>More activity to load · </span>}
        {teamLoading && <span>Loading team activity · </span>}
        {summary.nouns.map((noun, index) => <span key={noun.key}>
          {index > 0 && " · "}{noun.text}{noun.names.length > 0 && <> <span className="names">{noun.names.join(", ")}{noun.more > 0 ? ` … ${noun.more} more` : ""}</span></>}
        </span>)}
      </span>
      <span className="tr-line__r">
        {summary.failed > 0 && <b>{summary.failed} failed</b>}
        {summary.interrupted > 0 && <span>{summary.interrupted} interrupted</span>}
        {summary.cancelled > 0 && <span>{summary.cancelled} cancelled</span>}
        {running && <span className="tr-activity__running"><i aria-hidden="true" />Running</span>}
        {range !== null && <span className="t">{range}</span>}
      </span>
    </button>
    <Attachments facts={images} onOpenAttachment={openImage} onPreview={onPreview} reveal={reveal} />
    <ActivityImagesContext.Provider value={openImage}>
      <div id={listId} className="tr-activity__list" hidden={!expanded}>
        {partial && <div className="tr-activity__loading" role="status">
          {earlier.kind === "fetching" || earlier.kind === "revealing" ? "Loading earlier activity…" : <>
            <span>{earlier.kind === "failed" ? "Could not load earlier activity." : "This activity continues in earlier history."}</span>
            <button type="button" onClick={onLoadEarlier}>{earlier.kind === "failed" ? "Retry" : "Load more activity"}</button>
          </>}
        </div>}
        {(teamLoading || teamError !== null) && <div className="tr-activity__loading" role="status">
          <span>{teamError !== null ? "Could not load team messages." : "Loading team messages…"}</span>
          {teamError !== null && <button type="button" onClick={onRetryTeam}>Retry</button>}
        </div>}
        {children}
      </div>
    </ActivityImagesContext.Provider>
  </div>;
}

/** The second disclosure is independent of the room's outer fold preference. */
export function ActivityEvent({ domId, timelineId, label, summary, at, glyph, hit = false, forceOpen = false, failed = false, children }: {
  readonly domId: string; readonly timelineId: string; readonly label: string;
  readonly summary?: ReactNode; readonly at?: number | null; readonly glyph?: ReactNode;
  readonly hit?: boolean; readonly forceOpen?: boolean; readonly failed?: boolean; readonly children: ReactNode;
}) {
  const [open, setOpen] = useRowDisclosure(domId);
  useEffect(() => { if (hit || forceOpen) setOpen(true); }, [hit, forceOpen, setOpen]);
  return <details className="tr-activity-event" id={domId} data-timeline-id={timelineId}
    data-hit={hit || undefined} data-failed={failed || undefined} open={open || forceOpen}
    onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>
      <span className="tr-activity-event__glyph">{glyph ?? <FamilyGlyph glyph="doc" />}</span>
      <span className="tr-activity-event__label"><strong>{label}</strong>{summary !== undefined && <span>{summary}</span>}</span>
      {at != null && <time>{clock(at)}</time>}
      <span className="chev"><LineChevron /></span>
    </summary>
    {(open || forceOpen) && <div className="tr-activity-event__body">{children}</div>}
  </details>;
}

export function ContextEvent({ segment, domId, hit, forceOpen, onReceipt }: {
  readonly segment: Extract<Segment, { kind: "marker" | "summary" }>;
  readonly domId: string; readonly hit: boolean; readonly forceOpen: boolean;
  readonly onReceipt: (text: string) => void;
}) {
  const summary = segment.kind === "summary";
  const label = summary ? `${segment.purpose} summary` : "Session notice";
  const canonical = segment.row.body.type === "summary" ? segment.row.body.text
    : segment.kind === "marker" && segment.notice.type === "command_output" ? segment.notice.output : null;
  return <ActivityEvent domId={domId} timelineId={segment.row.timeline_id} label={label}
    summary={summary ? undefined : segment.text} at={segment.at} hit={hit} forceOpen={forceOpen}
    failed={segment.kind === "marker" && segment.tone === "failed"}>
    <div className="tr-prose"><MessagePreview messageKey={domId} role="agent" title={label}
      matched={hit}
      text={segment.text} recordedText={canonical} onReceipt={onReceipt} /></div>
  </ActivityEvent>;
}
