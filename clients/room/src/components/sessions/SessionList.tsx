import type { SessionRow } from "@vibefield/rosetta-protocol";
import { dayLabel } from "../../lib/format";
import { sessionLastActive } from "../../lib/session-activity";
import { useListMotion } from "../../lib/use-list-motion";
import { activityDay, compareNotImported } from "../../lib/activity-order";
import {
  arrivalsLabel, importStanding, newestReadableSource, NOT_IMPORTED_LABEL, notImportedAround, STILL_IMPORTING_LABEL,
} from "../../lib/import-state";
import { useDeferredListOrder } from "../../lib/use-deferred-list-order";
import type { ListPosition } from "../../lib/use-deferred-list-order";
import { ListActivityNotice } from "../ListActivityNotice";
import { FoldLine, SectionHead, useOpenFolds } from "../ListSections";
import type { ArrangedSection } from "../../lib/sections";
import { EmptyState } from "../ui";
import { SessionListItem } from "./SessionListItem";
import type { SessionFacts } from "./types";

export interface SessionListProps {
  /** Sessions in display order, normally newest first. */
  readonly sessions: readonly SessionRow[];
  readonly factsBySessionId: ReadonlyMap<string, SessionFacts>;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly selectedSessionId: string | null;
  readonly currentTimestamp: number;
  /** A backfill is running — unindexed sessions cook rather than lie, and
   * the ones it has not reached are drawn in date-free groups, "Still
   * importing" on top and "Not imported yet" at the foot. */
  readonly duringBackfill: boolean;
  /** Ids some session names as its parent (`parentIds`): a Pending row with
   * children and no metadata of its own has no transcript to import. */
  readonly parentIds?: ReadonlySet<string> | undefined;
  /** The census is still naming sessions: the empty state yields to the
   * section's veil, because only the ready may claim emptiness. */
  readonly pending: boolean;
  readonly emptyState?: {
    readonly title: string;
    readonly description: string;
  } | undefined;
  readonly onSessionSelect: (session: SessionRow) => void;
  /** The host's sections (R3), arranged; null draws the days it always did. */
  readonly sections?: readonly ArrangedSection<SessionRow>[] | null | undefined;
}

const sessionId = (session: SessionRow) => session.id;
const NO_PARENTS: ReadonlySet<string> = new Set();

interface SessionDayGroup {
  readonly day: number | null;
  /** One of the import's date-free not-imported groups. */
  readonly pending: boolean;
  /** That group is the top one, "Still importing"; else the foot's. */
  readonly importing: boolean;
  readonly label: string;
  readonly sessions: SessionRow[];
}

function groupSessionsByDay(
  positions: readonly ListPosition[],
  sessions: ReadonlyMap<string, SessionRow>,
  currentTimestamp: number,
): SessionDayGroup[] {
  const groups: SessionDayGroup[] = [];

  for (const { id, day, pending = false, importing = false } of positions) {
    const session = sessions.get(id);
    if (!session) continue;
    const currentGroup = groups.at(-1);
    if (currentGroup?.day === day && currentGroup.pending === pending && currentGroup.importing === importing) {
      currentGroup.sessions.push(session);
    } else {
      const label = !pending ? dayLabel(day, currentTimestamp) : importing ? STILL_IMPORTING_LABEL : NOT_IMPORTED_LABEL;
      groups.push({ day, pending, importing, label, sessions: [session] });
    }
  }

  // A not-imported head counts its rows: "Still importing · 2".
  return groups.map(group => group.pending
    ? { ...group, label: `${group.label} · ${group.sessions.length.toLocaleString()}` } : group);
}

export function SessionList(props: SessionListProps) {
  const scope = `browse:${props.sessions[0]?.project_id ?? "empty"}`;
  const sections = props.sections ?? null;
  const folds = useOpenFolds(sections, sessionId, props.selectedSessionId);
  const parents = props.parentIds ?? NO_PARENTS;
  const standing = (session: SessionRow) => importStanding(session, props.duringBackfill, parents.has(session.id));
  const notImported = (session: SessionRow) => standing(session) === "not-imported";
  // The sessions the import has not reached have no recorded activity to
  // rank or date them, so they sit in two date-free groups, newest source
  // first: the ones newer than every readable row above those rows ("Still
  // importing"), the rest at the foot ("Not imported yet"). A flip then
  // moves a top row a short way down into its day.
  const newestReadable = newestReadableSource(props.sessions, session => standing(session) === null);
  const stillImporting = (session: SessionRow) => newestReadable === null || session.updated_at > newestReadable;
  const position = (session: SessionRow): ListPosition => !notImported(session)
    ? { id: session.id, day: activityDay(sessionLastActive(session)) }
    : stillImporting(session) ? { id: session.id, day: null, pending: true, importing: true }
    : { id: session.id, day: null, pending: true };
  const roomOrder = (rows: readonly SessionRow[]) => notImportedAround(rows, notImported, stillImporting, compareNotImported);
  // A folded section's rows are the reader's to open, so they never wait on
  // the held geometry: only the open runs go through it.
  const layout = useDeferredListOrder(sections === null
    ? roomOrder(props.sessions).map(position)
    : sections.flatMap(entry => entry.section.fold !== undefined ? [] :
        (entry.order === "room" ? roomOrder(entry.rows) : entry.rows)
          .map(session => ({ ...position(session), section: entry.index }))), scope);
  const byId = new Map(props.sessions.map(session => [session.id, session]));
  const days = groupSessionsByDay(layout.items, byId, props.currentTimestamp);
  const motionRef = useListMotion(JSON.stringify(layout.items), scope, layout.scrollRef);
  const drawRow = (session: SessionRow) => (
    <SessionListItem
      key={`session:${session.id}`}
      session={session}
      facts={props.factsBySessionId.get(session.id) ?? null}
      runtimeKind={props.getRuntimeKind(session.runtime_id)}
      isSelected={session.id === props.selectedSessionId}
      currentTimestamp={props.currentTimestamp}
      duringBackfill={props.duringBackfill}
      metadataOnly={standing(session) === "unavailable"}
      onSessionSelect={props.onSessionSelect}
    />
  );
  // A held group whose rows moved on says why until the reader applies the
  // new order: a not-imported group counts the rows the import opened, and
  // only a new date is "Activity updated".
  const head = (group: SessionDayGroup) => {
    if (group.pending) {
      const imported = group.sessions.filter(session => !notImported(session)).length;
      return imported > 0 ? arrivalsLabel({ count: imported, older: false }) : group.label;
    }
    return group.sessions.some(session => notImported(session) || activityDay(sessionLastActive(session)) !== group.day)
      ? "Activity updated" : group.label;
  };
  // Headers and rows share one key space. Moving from Yesterday to Today
  // preserves the actual button, its keyboard focus and its animation.
  const drawDays = (groups: readonly SessionDayGroup[], keyPrefix: string) => groups.flatMap((group) => [
    <div key={`day:${keyPrefix}${!group.pending ? group.day : group.importing ? "still-importing" : "not-imported"}`} className="ah-day"
      data-not-imported={!group.pending ? undefined : group.importing ? "top" : "foot"}>
      {head(group)}
    </div>,
    ...group.sessions.map(drawRow),
  ]);
  // An open run draws the held geometry; a folded one, the room's order now.
  const drawRun = (entry: ArrangedSection<SessionRow>, positions: readonly ListPosition[]) =>
    entry.order === "room"
      ? drawDays(groupSessionsByDay(positions, byId, props.currentTimestamp), `${entry.index}:`)
      : positions.flatMap(item => byId.get(item.id) ?? []).map(drawRow);
  const sectioned = sections === null ? null : sections.flatMap((entry) => {
    if (entry.section.fold !== undefined) {
      if (entry.rows.length === 0 && entry.section.fold.count === undefined) return [];
      const open = folds.isOpen(entry.section.key);
      return [
        <FoldLine key={`section:${entry.section.key}`} arranged={entry} open={open}
          onToggle={() => folds.toggle(entry.section.key)} />,
        ...(open ? drawRun(entry, (entry.order === "room" ? roomOrder(entry.rows) : entry.rows).map(position)) : []),
      ];
    }
    const positions = layout.items.filter(item => item.section === entry.index && byId.has(item.id));
    if (positions.length === 0) return [];
    return [
      ...(entry.section.title === undefined ? [] : [
        <SectionHead key={`section:${entry.section.key}`} sectionKey={entry.section.key}>{entry.section.title}</SectionHead>,
      ]),
      ...drawRun(entry, positions),
    ];
  });

  return (
    <div className="ah-list-view" {...layout.hostProps}>
      <div className="ah-scroll ah-session-list ah-motion-list" ref={motionRef} onScroll={layout.onScroll}>
        {sectioned ?? drawDays(days, "")}
        {props.sessions.length === 0 && !props.pending && (
          <EmptyState
            title={props.emptyState?.title ?? "No sessions here yet"}
            description={
              props.emptyState?.description ??
              "Sessions appear as rosettad reads this project's agent stores."
            }
          />
        )}
      </div>
      <ListActivityNotice pending={layout.pending} onShow={layout.apply} noun="sessions" arrivals={layout.arrivals} />
    </div>
  );
}
