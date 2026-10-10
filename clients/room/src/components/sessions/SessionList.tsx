import type { SessionRow } from "@rosetta/protocol";
import { dayLabel } from "../../lib/format";
import { sessionLastActive } from "../../lib/session-activity";
import { useListMotion } from "../../lib/use-list-motion";
import { activityDay } from "../../lib/activity-order";
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
  /** A backfill is running — unindexed sessions cook rather than lie. */
  readonly duringBackfill: boolean;
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

interface SessionDayGroup {
  readonly day: number | null;
  readonly label: string;
  readonly sessions: SessionRow[];
}

function groupSessionsByDay(
  positions: readonly ListPosition[],
  sessions: ReadonlyMap<string, SessionRow>,
  currentTimestamp: number,
): SessionDayGroup[] {
  const groups: SessionDayGroup[] = [];

  for (const { id, day } of positions) {
    const session = sessions.get(id);
    if (!session) continue;
    const label = dayLabel(day, currentTimestamp);
    const currentGroup = groups.at(-1);
    if (currentGroup?.day === day) {
      currentGroup.sessions.push(session);
    } else {
      groups.push({ day, label, sessions: [session] });
    }
  }

  return groups;
}

export function SessionList(props: SessionListProps) {
  const scope = `browse:${props.sessions[0]?.project_id ?? "empty"}`;
  const sections = props.sections ?? null;
  const folds = useOpenFolds(sections, sessionId, props.selectedSessionId);
  const position = (session: SessionRow) => ({ id: session.id, day: activityDay(sessionLastActive(session)) });
  // A folded section's rows are the reader's to open, so they never wait on
  // the held geometry: only the open runs go through it.
  const layout = useDeferredListOrder(sections === null
    ? props.sessions.map(position)
    : sections.flatMap(entry => entry.section.fold !== undefined ? [] :
        entry.rows.map(session => ({ ...position(session), section: entry.index }))), scope);
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
      onSessionSelect={props.onSessionSelect}
    />
  );
  // Headers and rows share one key space. Moving from Yesterday to Today
  // preserves the actual button, its keyboard focus and its animation.
  const drawDays = (groups: readonly SessionDayGroup[], keyPrefix: string) => groups.flatMap((group) => [
    <div key={`day:${keyPrefix}${group.day}`} className="ah-day">
      {group.sessions.some(session => activityDay(sessionLastActive(session)) !== group.day)
        ? "Activity updated" : group.label}
    </div>,
    ...group.sessions.map(drawRow),
  ]);
  const drawRun = (entry: ArrangedSection<SessionRow>, rows: readonly SessionRow[]) =>
    entry.order === "room"
      ? drawDays(groupSessionsByDay(rows.map(position), byId, props.currentTimestamp), `${entry.index}:`)
      : rows.map(drawRow);
  const sectioned = sections === null ? null : sections.flatMap((entry) => {
    if (entry.section.fold !== undefined) {
      if (entry.rows.length === 0 && entry.section.fold.count === undefined) return [];
      const open = folds.isOpen(entry.section.key);
      return [
        <FoldLine key={`section:${entry.section.key}`} arranged={entry} open={open}
          onToggle={() => folds.toggle(entry.section.key)} />,
        ...(open ? drawRun(entry, entry.rows) : []),
      ];
    }
    const rows = layout.items.flatMap(item => item.section === entry.index ? byId.get(item.id) ?? [] : []);
    if (rows.length === 0) return [];
    return [
      ...(entry.section.title === undefined ? [] : [
        <SectionHead key={`section:${entry.section.key}`} sectionKey={entry.section.key}>{entry.section.title}</SectionHead>,
      ]),
      ...drawRun(entry, rows),
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
      <ListActivityNotice pending={layout.pending} onShow={layout.apply} noun="sessions" />
    </div>
  );
}
