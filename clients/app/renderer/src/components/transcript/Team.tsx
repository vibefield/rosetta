// Agent Teams — an addressable roster and shared boundary messages.
//
// Participant colour is intentionally confined to Claude Code's small mark.
// It identifies an @address; it never paints a rail, surface, status, or prose.
import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import type {
  CollaborationEvent,
  ParticipantColor,
  ResponseRuntime,
  SessionRow,
  TimelineEntry,
} from "@rosetta/protocol";
import { AgentMark } from "../../lib/agents";
import { clock } from "../../lib/format";
import type {
  CollaborationTimeBasis,
  RuntimeProfile,
  TeamMemberPresentation,
  TeamParticipantPresentation,
} from "../../lib/transcript";
import { canonicalText, firstLine, runtimeProfile, spanText } from "../../lib/transcript";
import { Prose } from "./Prose";
import { LineChevron } from "./glyphs";

function address(name: string | null): string {
  if (name === null || name.trim() === "") return "teammate";
  return name.startsWith("@") ? name : `@${name}`;
}

function speaker(name: string | null): string {
  return name === "coordinator" ? "Claude Code" : address(name);
}

function TeamAvatar({
  color,
  name,
}: {
  readonly color: ParticipantColor | null;
  readonly name: string | null;
}) {
  return (
    <span
      className="tm-avatar"
      data-member-color={color ?? undefined}
      aria-hidden="true"
    >
      <AgentMark kind="claude-code" note={name === null ? "Agent Team member" : speaker(name)} />
    </span>
  );
}

export function TeamWorkthread({
  name,
  color,
  runtime,
  startedAt,
  endedAt,
  children,
}: {
  readonly name: string | null;
  readonly color: ParticipantColor | null;
  readonly runtime: RuntimeProfile;
  readonly startedAt: number | null;
  readonly endedAt: number | null;
  readonly children: ReactNode;
}) {
  const range =
    startedAt === null
      ? endedAt === null
        ? null
        : clock(endedAt)
      : endedAt === null || endedAt === startedAt
        ? clock(startedAt)
        : `${clock(startedAt)} → ${clock(endedAt)}`;
  const duration =
    startedAt !== null && endedAt !== null && endedAt >= startedAt
      ? spanText(endedAt - startedAt)
      : null;
  return (
    <div
      className="tr-sub tm-workthread"
      data-anchored="true"
      data-member-color={color ?? undefined}
      role="region"
      aria-label={`Proven work behind ${address(name)} result`}
    >
      <div className="tr-sub__head">
        <TeamAvatar color={color} name={name} />
        <span>
          <span className="tm-workthread__name">{address(name)}</span>
          <span className="tm-workthread__kind"> · exact task-bounded work slice</span>
        </span>
        {range !== null && <span className="when">{range}</span>}
      </div>
      <div className="tr">
        <div className="tr-head">
          <span className="mark"><TeamAvatar color={color} name={name} /></span>
          <span className="tm-workthread__name">{address(name)}</span>
          <span className="model">{runtime.label}</span>
          {duration !== null && <span className="facts">{duration}</span>}
        </div>
        {children}
        <div className="tm-workthread__end">
          <span>result rendered above · nested delivery copy suppressed</span>
          {endedAt !== null && <time>{clock(endedAt)}</time>}
        </div>
      </div>
    </div>
  );
}

function TeamMention({ member }: { readonly member: TeamMemberPresentation }) {
  return <AddressMention name={member.name} color={member.color} />;
}

function AddressMention({
  name,
  color,
}: {
  readonly name: string | null;
  readonly color: ParticipantColor | null;
}) {
  return (
    <span className="tm-mention" data-member-color={color ?? undefined}>
      <TeamAvatar color={color} name={name} />
      {speaker(name)}
    </span>
  );
}

function invocationRange(members: readonly TeamMemberPresentation[]): string | null {
  const stamps = members
    .map((member) => member.row.at)
    .filter((value): value is number => value !== null);
  if (stamps.length === 0) return null;
  const first = Math.min(...stamps);
  const last = Math.max(...stamps);
  return first === last ? clock(first) : `${clock(first)}–${clock(last)}`;
}

export interface TeamInvocationProps {
  readonly members: readonly TeamMemberPresentation[];
  readonly domIdOf: (timelineId: string) => string;
  readonly highlightedTimelineId: string | null;
  readonly focusTimelineId: string | null;
}

/** One proven initial burst, or the first safe view of an older team, becomes
 * one roster line. Later add/resume operations keep their own chronological
 * line and put the member's model/effort at the edge. */
export function TeamInvocation({
  members,
  domIdOf,
  highlightedTimelineId,
  focusTimelineId,
}: TeamInvocationProps) {
  const first = members[0];
  const isRoster = first.operation === "start" || first.operation === "known";
  const forceOpen =
    isRoster && members.some((member) => member.row.timeline_id === focusTimelineId);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (forceOpen) setOpen(true);
  }, [forceOpen]);
  const expanded = isRoster && (open || forceOpen);
  const range = invocationRange(members);
  const hit = members.some((member) => member.row.timeline_id === highlightedTimelineId);

  if (!isRoster) {
    const verb = first.operation === "resume" ? "Resumed teammate" : "Added teammate";
    return (
      <div
        className="tr-line tm-invocation"
        id={domIdOf(first.row.timeline_id)}
        data-timeline-id={first.row.timeline_id}
        data-team-operation={first.operation}
        data-member-color={first.color ?? undefined}
        data-hit={hit || undefined}
      >
        <span className="tr-line__g">
          <TeamAvatar color={first.color} name={first.name} />
        </span>
        <span className="tr-line__v">{verb}</span>
        <span className="tr-line__o" title={first.objective}>
          <TeamMention member={first} />
          <span className="in"> · {first.objective}</span>
        </span>
        <span className="tr-line__r">
          <span className="tm-runtime">{first.runtime.label}</span>
          {range !== null && <span className="t">{range}</span>}
        </span>
      </div>
    );
  }

  return (
    <>
      <div
        className="tr-line tm-invocation"
        id={domIdOf(first.row.timeline_id)}
        data-timeline-id={first.row.timeline_id}
        data-team-operation={first.operation}
        data-hit={hit || undefined}
        role="button"
        tabIndex={0}
        aria-expanded={expanded}
        onClick={() => setOpen((value) => !value)}
        onKeyDown={(event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          setOpen((value) => !value);
        }}
      >
        <span className="tr-line__g tm-avatar-stack" aria-label="Agent Team members">
          {members.slice(0, 3).map((member) => (
            <TeamAvatar key={member.row.timeline_id} color={member.color} name={member.name} />
          ))}
        </span>
        <span className="tr-line__v">
          {first.operation === "start" ? "Started team" : "Team active"}
        </span>
        <span className="tr-line__o">{first.team}</span>
        <span className="tr-line__r">
          <span>
            {members.length} {first.operation === "known" ? "known " : ""}
            {members.length === 1 ? "teammate" : "teammates"}
          </span>
          {range !== null && <span className="t">{range}</span>}
          <span className="chev"><LineChevron /></span>
        </span>
      </div>
      {expanded && (
        <div className="tm-roster" role="region" aria-label={`${first.team} roster`}>
          {members.map((member, index) => (
            <div
              className="tm-roster__row"
              key={member.row.timeline_id}
              id={index === 0 ? undefined : domIdOf(member.row.timeline_id)}
              data-timeline-id={member.row.timeline_id}
              data-member-color={member.color ?? undefined}
              data-hit={member.row.timeline_id === highlightedTimelineId || undefined}
            >
              <TeamAvatar color={member.color} name={member.name} />
              <span className="tm-roster__name">{address(member.name)}</span>
              <span className="tm-roster__job" title={member.objective}>{member.objective}</span>
              <span className="tm-runtime">{member.runtime.label}</span>
            </div>
          ))}
          {members.some((member) => member.color === null) && (
            <div className="tm-note">A missing native colour keeps the Claude mark neutral.</div>
          )}
          {first.operation === "known" && (
            <div className="tm-note">
              Team creation is above this page; this is the known roster, not a reconstructed start.
            </div>
          )}
        </div>
      )}
    </>
  );
}

/** A page can begin after the creation calls while metadata still proves the
 * current roster. State is shown without fabricating a historical “Started”. */
export function KnownTeamRoster({
  sessions,
  rowsBySession,
  responseRuntimesBySession,
  historyReady,
}: {
  readonly sessions: readonly SessionRow[];
  readonly rowsBySession: ReadonlyMap<string, readonly TimelineEntry[]>;
  readonly responseRuntimesBySession: ReadonlyMap<string, readonly ResponseRuntime[]>;
  readonly historyReady: boolean;
}) {
  const [open, setOpen] = useState(false);
  const team = sessions.find((session) => session.delegation?.team != null)?.delegation?.team;
  return (
    <div className="tr-ledger tm-known">
      <div
        className="tr-line"
        role="button"
        tabIndex={0}
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        onKeyDown={(event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          setOpen((value) => !value);
        }}
      >
        <span className="tr-line__g tm-avatar-stack">
          {sessions.slice(0, 3).map((session) => (
            <TeamAvatar
              key={session.id}
              color={session.delegation?.color ?? null}
              name={session.title}
            />
          ))}
        </span>
        <span className="tr-line__v">Team active</span>
        <span className="tr-line__o">
          {team == null ? "Agent Team" : team} · {sessions.length} known {sessions.length === 1 ? "teammate" : "teammates"}
        </span>
        <span className="tr-line__r"><span className="chev"><LineChevron /></span></span>
      </div>
      {open && (
        <div className="tm-roster" role="region" aria-label="Known Agent Team members">
          {sessions.map((session) => (
            <div className="tm-roster__row" key={session.id}>
              <TeamAvatar
                color={session.delegation?.color ?? null}
                name={session.title}
              />
              <span className="tm-roster__name">{address(session.title ?? session.native_session_id)}</span>
              <span className="tm-roster__job" title={session.summary ?? undefined}>{session.summary ?? "objective not recorded on this page"}</span>
              <span className="tm-runtime">
                {runtimeProfile(
                  rowsBySession.get(session.id) ?? [],
                  session.delegation?.configured_model ?? null,
                  responseRuntimesBySession.get(session.id) ?? [],
                ).label}
              </span>
            </div>
          ))}
          <div className="tm-note">
            {historyReady
              ? "Creation is above this page; no historical start time is inferred."
              : "Loading complete delivered-message history; private teammate work stays closed."}
          </div>
        </div>
      )}
    </div>
  );
}

function eventBody(event: CollaborationEvent): string | null {
  if (event.body !== null) return canonicalText(event.body);
  return event.summary;
}

function eventRoute(event: CollaborationEvent): string | null {
  if (event.recipient !== null) return `→ ${speaker(event.recipient)}`;
  if (event.team !== null) return `→ @${event.team}`;
  return null;
}

function eventRuntime(participant: TeamParticipantPresentation | null): RuntimeProfile {
  return participant?.runtime ?? {
    basis: "unknown",
    label: "runtime not recorded",
    model: null,
    effort: null,
    profileCount: 0,
  };
}

function TeamTime({
  at,
  basis,
  className,
}: {
  readonly at: number | null;
  readonly basis: CollaborationTimeBasis;
  readonly className?: string;
}) {
  if (at === null) return null;
  const arrivalFallback = basis === "observed_arrival";
  return (
    <time
      className={className}
      data-time-basis={basis}
      title={
        arrivalFallback
          ? "Delivery arrival time; native authored time was not recorded"
          : "Native authored time"
      }
    >
      {clock(at)}
      {arrivalFallback && <span className="tm-time-basis"> · arrived</span>}
    </time>
  );
}

function lifecycleText(event: CollaborationEvent): string {
  const who = speaker(event.sender);
  switch (event.kind) {
    case "idle":
      return event.status === "failed" ? `${who} is idle · failed` : `${who} is available`;
    case "stop_requested":
      return `stop requested by ${who}`;
    case "stop_approved":
      return `stop approved by ${who}`;
    case "terminated":
      return `${who} terminated`;
    case "assignment":
    case "message":
    case "report":
      return event.summary ?? event.kind;
  }
}

function TeamReport({
  event,
  at,
  timeBasis,
  domId,
  timelineId,
  color,
  runtime,
  hit,
  forceOpen,
  work,
  onReceipt,
}: {
  readonly event: CollaborationEvent;
  readonly at: number | null;
  readonly timeBasis: CollaborationTimeBasis;
  readonly domId: string;
  readonly timelineId: string;
  readonly color: ParticipantColor | null;
  readonly runtime: RuntimeProfile;
  readonly hit: boolean;
  readonly forceOpen: boolean;
  readonly work: TeamReportWorkPresentation | null;
  readonly onReceipt: (message: string) => void;
}) {
  const body = eventBody(event);
  const [open, setOpen] = useState(false);
  const [workOpen, setWorkOpen] = useState(false);
  useEffect(() => {
    if (forceOpen) setOpen(true);
  }, [forceOpen]);
  const pressable = body !== null;
  const expanded = pressable && (open || forceOpen);
  const route = eventRoute(event);
  const summary = event.summary ?? (body === null ? "result" : firstLine(body));
  const workSpan =
    work?.startedAt !== null && work?.startedAt !== undefined && work.endedAt !== null
      ? spanText(Math.max(0, work.endedAt - work.startedAt))
      : null;
  const workFacts =
    work === null
      ? null
      : [work.actionCount === null || work.actionCount === 0 ? "recorded work" : `${work.actionCount} ${work.actionCount === 1 ? "action" : "actions"}`, workSpan]
          .filter((fact): fact is string => fact !== null)
          .join(" · ");
  return (
    <div className="tr-ledger tm-report" data-member-color={color ?? undefined}>
      <div
        className="tr-line tm-report__line"
        id={domId}
        data-timeline-id={timelineId}
        data-hit={hit || undefined}
        role={pressable ? "button" : undefined}
        tabIndex={pressable ? 0 : undefined}
        aria-expanded={pressable ? expanded : undefined}
        onClick={() => pressable && setOpen((value) => !value)}
        onKeyDown={(keyboardEvent) => {
          if (!pressable || (keyboardEvent.key !== "Enter" && keyboardEvent.key !== " ")) return;
          keyboardEvent.preventDefault();
          setOpen((value) => !value);
        }}
      >
        <span className="tr-line__g"><TeamAvatar color={color} name={event.sender} /></span>
        <span className="tr-line__v">Reported</span>
        <span className="tr-line__o" title={summary}>
          {address(event.sender)}{route === null ? "" : ` ${route}`} <span className="in">· {summary}</span>
        </span>
        <span className="tr-line__r">
          <span className="tm-runtime">{runtime.label}</span>
          <TeamTime at={at} basis={timeBasis} className="t" />
          {pressable && <span className="chev"><LineChevron /></span>}
        </span>
      </div>
      {expanded && (
        <div className="tr-detail tm-report__detail" role="region" aria-label={`Report from ${address(event.sender)}`}>
          <div className="tr-detail__bar">
            <span className="path">
              {event.task_id === null ? "correlated team result" : `task result · ${event.task_id}`}
            </span>
            <span>one rendered delivery</span>
          </div>
          {body !== null && <div className="tm-message__body"><Prose text={body} onReceipt={onReceipt} /></div>}
          {work !== null && (
            <div className="tm-message__foot">
              <button
                className="tm-work"
                type="button"
                aria-expanded={workOpen}
                onClick={() => setWorkOpen((value) => !value)}
              >
                <span className="chev" aria-hidden="true"><LineChevron /></span>
                <span>proven work behind this result</span>
              </button>
              {workFacts !== null && <span>{workFacts}</span>}
            </div>
          )}
          {workOpen && work?.content}
        </div>
      )}
    </div>
  );
}

export interface TeamEventProps {
  readonly event: CollaborationEvent;
  readonly at: number | null;
  readonly timeBasis: CollaborationTimeBasis;
  readonly domId: string;
  readonly timelineId: string;
  readonly participant: TeamParticipantPresentation | null;
  readonly recipientParticipant?: TeamParticipantPresentation | null;
  readonly hit: boolean;
  readonly forceOpen: boolean;
  readonly reportWork?: TeamReportWorkPresentation | null;
  readonly onReceipt: (message: string) => void;
}

export interface TeamReportWorkPresentation {
  readonly content: ReactNode;
  readonly actionCount: number | null;
  readonly startedAt: number | null;
  readonly endedAt: number | null;
}

export function TeamEvent({
  event,
  at,
  timeBasis,
  domId,
  timelineId,
  participant,
  recipientParticipant = null,
  hit,
  forceOpen,
  reportWork = null,
  onReceipt,
}: TeamEventProps) {
  const color = event.color ?? participant?.color ?? null;
  const runtime = eventRuntime(participant);
  if (event.kind === "report") {
    return (
      <TeamReport
        event={event}
        at={at}
        timeBasis={timeBasis}
        domId={domId}
        timelineId={timelineId}
        color={color}
        runtime={runtime}
        hit={hit}
        forceOpen={forceOpen}
        work={reportWork}
        onReceipt={onReceipt}
      />
    );
  }

  if (!matchesMessage(event)) {
    const failed = event.status === "failed";
    return (
      <div
        className={`tr-mark tm-lifecycle${failed ? " tr-mark--error" : ""}`}
        id={domId}
        data-timeline-id={timelineId}
        data-hit={hit || undefined}
      >
        <TeamAvatar color={color} name={event.sender} />
        <span>{lifecycleText(event)}</span>
        <TeamTime at={at} basis={timeBasis} />
      </div>
    );
  }

  const body = eventBody(event);
  const route = eventRoute(event);
  return (
    <article
      className="tm-message"
      id={domId}
      data-timeline-id={timelineId}
      data-member-color={color ?? undefined}
      data-hit={hit || undefined}
    >
      <header className="tm-message__head">
        <span className="mark"><TeamAvatar color={color} name={event.sender} /></span>
        <span className="tm-message__who">{speaker(event.sender)}</span>
        {event.recipient !== null ? (
          <span className="tm-message__route">
            → <AddressMention name={event.recipient} color={recipientParticipant?.color ?? null} />
          </span>
        ) : route !== null ? (
          <span className="tm-message__route">{route}</span>
        ) : null}
        <span className="tm-message__kind">{event.kind}</span>
        <TeamTime at={at} basis={timeBasis} className="tm-message__time" />
      </header>
      {event.summary !== null && event.body !== null && (
        <p className="tm-message__subject">{event.summary}</p>
      )}
      {body !== null && <div className="tm-message__body"><Prose text={body} onReceipt={onReceipt} /></div>}
    </article>
  );
}

function matchesMessage(event: CollaborationEvent): boolean {
  return event.kind === "message" || event.kind === "assignment";
}
