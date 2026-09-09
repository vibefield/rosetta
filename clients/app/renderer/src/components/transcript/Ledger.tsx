// T-2 / T-3 / §03 D — the activity ledger.
//
// The shipped work block's closed state was counts: `bash 4 · read 1` answers
// "how much machinery", which is not what anyone came to History for. This is
// its replacement: a sentence you can read without opening — verb · object ·
// result — on a 1px rail at the tree's own row pitch, with no fill and no
// border, because the reply's prose is the primary content and machinery is a
// footnote.
//
// Three rules live here and nowhere else:
//   T-3  looking folds, acts stay, a failure breaks the fold. `ledger.ts` did
//        the folding; this draws the result.
//   R-3  every activity run, including a single line, closes to its head — one
//        sentence of nouns, acts first, the changed files named, the failure count in
//        red at the right beside the span (amended 2026-09-09). The ladder
//        stays in the DOM, and the column head's activity act reveals every
//        run at once while keeping each head as its title.
//   R-5  a live run's head takes the breathing dot and carries the line that
//        is happening, in green; nothing opens under the reader's eye.
//   T-5  hue only for what did not complete or is still happening. The green
//        breath is drawn ONLY in a live session below the watermark — a
//        `running` row in a finished transcript is a record that stopped, and
//        it reads muted rather than pretending to be alive.
import type { ReactNode } from "react";
import { Fragment, useEffect, useRef, useState } from "react";
import type { TimelineEntry } from "@rosetta/protocol";
import { clock } from "../../lib/format";
import type {
  LedgerLine,
  LedgerMember,
  ObjectPart,
  PathFact,
  RunSummary,
  TeamMemberPresentation,
  WorkflowAttemptFact,
} from "../../lib/transcript";
import { objectText, spanText } from "../../lib/transcript";
import { FaviconStack, Favicon } from "./Favicon";
import { FailGlyph, FamilyGlyph, LineChevron } from "./glyphs";
import { useFoldPreference } from "./fold-preference";
import { hasDetail, LineDetail } from "./LineDetail";
import { Marker, SummaryNote } from "./Marker";
import { TeamInvocation } from "./Team";
import { Thinking } from "./Thinking";
import type { WorkflowChildThread } from "./Workflow";

const TONE_CLASS: Readonly<Record<ObjectPart["tone"], string | undefined>> = {
  plain: undefined,
  dir: "dir",
  query: "q",
  aside: "in",
};

function ObjectParts({ parts }: { readonly parts: readonly ObjectPart[] }) {
  return (
    <>
      {parts.map((part, at) => {
        const className = TONE_CLASS[part.tone];
        return className === undefined ? (
          <span key={at}>{part.text}</span>
        ) : (
          <span className={className} key={at}>
            {part.text}
          </span>
        );
      })}
    </>
  );
}

export interface LineProps {
  readonly line: LedgerLine;
  readonly domId: string;
  readonly hit: boolean;
  /** A live session below the watermark: the only place the green breath is
   * honest (§04's status rows). */
  readonly alive: boolean;
  /** Force the line open — a search hit landed inside it. */
  readonly forceOpen: boolean;
  /** The child runs this line owns, already built (§03 F). */
  readonly subthreads: readonly WorkflowChildThread[];
  readonly workflowAttempts: readonly WorkflowAttemptFact[];
  readonly hasChildren: boolean;
  /** A later contact with an already-mounted worker navigates to the original
   * disclosure instead of opening an empty duplicate. */
  readonly onOpenLinkedChild: (() => void) | null;
  readonly onOpenPath: (path: PathFact) => void;
}

export function Line({
  line,
  domId,
  hit,
  alive,
  forceOpen,
  subthreads,
  workflowAttempts,
  hasChildren,
  onOpenLinkedChild,
  onOpenPath,
}: LineProps) {
  const [open, setOpen] = useState(line.openByDefault);
  useEffect(() => {
    if (forceOpen) setOpen(true);
  }, [forceOpen]);

  const navigatesToChild = onOpenLinkedChild !== null;
  const pressable = navigatesToChild || hasChildren || hasDetail(line.detail, line.diagnostic);
  const isOpen = !navigatesToChild && pressable && (open || forceOpen);
  const running = line.state === "running" && alive;
  const state =
    line.state === "failed"
      ? "failed"
      : line.state === "running"
        ? running
          ? "running"
          : "muted"
        : line.state === "muted"
          ? "muted"
          : undefined;

  const sources = line.detail.kind === "sources" ? line.detail.sources : null;
  const fetched = line.detail.kind === "fetch" ? line.detail.fetch : null;
  const stamp = line.at === null ? null : clock(line.at);

  const toggle = () => {
    if (navigatesToChild) {
      onOpenLinkedChild();
      return;
    }
    if (pressable) setOpen((value) => !value);
  };

  const content = (
    <>
      <div
        className="tr-line"
        id={domId}
        data-timeline-id={line.rows[0]?.timeline_id}
        data-state={state}
        data-hit={hit || undefined}
        data-web={line.family.startsWith("web_") ? line.family : undefined}
        data-page-title={fetched?.title ? true : undefined}
        role={pressable ? "button" : undefined}
        tabIndex={pressable ? 0 : undefined}
        aria-expanded={!navigatesToChild && pressable ? isOpen : undefined}
        aria-controls={
          !navigatesToChild && hasDetail(line.detail, line.diagnostic) ? `${domId}-detail` : undefined
        }
        aria-label={navigatesToChild ? `${line.verb}: open original worker subthread` : undefined}
        onClick={toggle}
        onKeyDown={(event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          toggle();
        }}
      >
        <span className="tr-line__g">
          {line.state === "failed" ? (
            <FailGlyph />
          ) : running ? (
            <i className="tr-dot" />
          ) : (
            <FamilyGlyph glyph={line.glyph} />
          )}
        </span>
        <span className="tr-line__v">{line.verb}</span>
        <span className="tr-line__o" title={objectText(line.object)}>
          <ObjectParts parts={line.object} />
        </span>
        <span className="tr-line__r">
          {sources !== null && sources.length > 0 && (
            <FaviconStack hosts={sources.map((source) => source.host)} />
          )}
          {line.result !== null && (
            <span
              className={
                line.resultTone === "failed" || line.resultTone === "running" ? "why" : undefined
              }
            >
              {line.result}
            </span>
          )}
          {fetched !== null && fetched.title !== null && (
            <>
              <Favicon host={fetched.host} />
              <span className="ttl" title={fetched.title}>{fetched.title}</span>
            </>
          )}
          {stamp !== null && <span className="t">{line.elided ? `→ ${stamp}` : stamp}</span>}
          {pressable && (
            <span className="chev">
              <LineChevron />
            </span>
          )}
        </span>
      </div>
      {isOpen && (
        <>
          {(() => {
            const body = (
              <LineDetail
                detail={line.detail}
                diagnostic={line.diagnostic}
                workflowChildren={line.detail.kind === "workflow" ? subthreads : []}
                workflowAttempts={workflowAttempts}
                onOpenPath={onOpenPath}
              />
            );
            // `LineDetail` answers null when the family has nothing honest to
            // show; a delegate whose child IS the detail is exactly that case.
            return hasDetail(line.detail, line.diagnostic) ? (
              <div className="tr-detail" id={`${domId}-detail`}>{body}</div>
            ) : null;
          })()}
          {line.detail.kind === "workflow" ? null : subthreads.map((thread) => thread.node)}
        </>
      )}
    </>
  );
  return line.family.startsWith("web_") ? <div className="tr-web">{content}</div> : content;
}

export interface LedgerProps {
  /** The ladder in order: lines, and the thoughts, markers and summaries
   * that happened between them (R-1). */
  readonly members: readonly LedgerMember[];
  readonly summary: RunSummary;
  /** The margin anchor of a closed run, drawn by the caller (T-4). */
  readonly anchor?: ReactNode;
  readonly domIdOf: (timelineId: string) => string;
  readonly highlightedTimelineId: string | null;
  /** The reader is being sent to a row inside this run. */
  readonly focusTimelineId: string | null;
  readonly alive: (line: LedgerLine) => boolean;
  readonly subthreadsFor: (line: LedgerLine) => readonly WorkflowChildThread[];
  readonly workflowAttemptsFor: (line: LedgerLine) => readonly WorkflowAttemptFact[];
  readonly hasChildren: (line: LedgerLine) => boolean;
  /** A search hit landed inside a CHILD run this line owns: the line has to
   * open for the subthread to mount at all, so the reader is not sent to an
   * element that does not exist yet. */
  readonly childFocused: (line: LedgerLine) => boolean;
  /** The already-anchored worker named by a later delegate/resume line. */
  readonly linkedChildId: (line: LedgerLine) => string | null;
  readonly onOpenLinkedChild: (childId: string) => void;
  /** A proven team delegate replaces the generic delegate sentence with its
   * roster operation. Ordinary Agent workers return null and keep Line. */
  readonly teamMemberFor: (line: LedgerLine) => TeamMemberPresentation | null;
  readonly onOpenPath: (path: PathFact) => void;
}

const TEAM_START_BURST_MS = 5 * 60_000;

function sameStartBurst(
  first: TeamMemberPresentation,
  candidate: TeamMemberPresentation,
): boolean {
  if (candidate.operation !== "add" || candidate.team !== first.team) return false;
  if (first.row.at === null || candidate.row.at === null) return true;
  return candidate.row.at >= first.row.at && candidate.row.at - first.row.at <= TEAM_START_BURST_MS;
}

function rowsOf(member: LedgerMember): readonly TimelineEntry[] {
  return member.kind === "line" ? member.line.rows : [member.row];
}

export function Ledger({
  members,
  summary,
  anchor,
  domIdOf,
  highlightedTimelineId,
  focusTimelineId,
  alive,
  subthreadsFor,
  workflowAttemptsFor,
  hasChildren,
  childFocused,
  linkedChildId,
  onOpenLinkedChild,
  teamMemberFor,
  onOpenPath,
}: LedgerProps) {
  const inside = (member: LedgerMember) =>
    (member.kind === "line" && childFocused(member.line)) ||
    (focusTimelineId !== null &&
      rowsOf(member).some((row) => row.timeline_id === focusTimelineId));
  const focusInside = members.some(inside);
  // §05 — the room's activity act is the DEFAULT every run follows: closed,
  // or open while the act holds every run open. A press on the head is an
  // override (`true`/`false`); the next press of the act clears it.
  const everyRunOpen = !useFoldPreference().folded;
  // Activity follows the room from its first line. A live run that grows
  // keeps any explicit override, so it never closes on its own (R-5).
  // Speech, such as a question, has no summary head and stays open.
  const [override, setOverride] = useState<boolean | null>(() =>
    summary.closedByDefault ? null : true,
  );
  const actRef = useRef(everyRunOpen);
  useEffect(() => {
    if (actRef.current === everyRunOpen) return;
    actRef.current = everyRunOpen;
    setOverride(null);
  }, [everyRunOpen]);
  useEffect(() => {
    if (focusInside) setOverride(true);
  }, [focusInside]);
  // A landing rings a row after scrolling to it, and the column scrolled
  // before this ladder was shown. Open on the ring, then bring the row in.
  const highlightInside =
    highlightedTimelineId !== null &&
    members.some((member) =>
      rowsOf(member).some((row) => row.timeline_id === highlightedTimelineId),
    );
  const domIdRef = useRef(domIdOf);
  domIdRef.current = domIdOf;
  useEffect(() => {
    if (!highlightInside || highlightedTimelineId === null) return;
    setOverride(true);
    const frame = window.requestAnimationFrame(() => {
      document
        .getElementById(domIdRef.current(highlightedTimelineId))
        ?.scrollIntoView({ block: "center" });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [highlightInside, highlightedTimelineId]);

  const drawn: ReactNode[] = [];
  for (let index = 0; index < members.length; index += 1) {
    const member = members[index];
    if (member.kind !== "line") {
      // Inside the ladder every member keeps the form it had as a segment.
      const rowId = member.row.timeline_id;
      const hit = highlightedTimelineId === rowId;
      if (member.kind === "thinking") {
        drawn.push(
          <Thinking
            key={member.key}
            text={member.text}
            spanMs={member.spanMs}
            domId={domIdOf(rowId)}
            timelineId={rowId}
            hit={hit}
            anchor={null}
          />,
        );
      } else if (member.kind === "marker") {
        drawn.push(
          <Marker
            key={member.key}
            text={member.text}
            tone={member.tone}
            domId={domIdOf(rowId)}
            timelineId={rowId}
            hit={hit}
          />,
        );
      } else {
        drawn.push(
          <SummaryNote
            key={member.key}
            purpose={member.purpose}
            text={member.text}
            domId={domIdOf(rowId)}
            timelineId={rowId}
            hit={hit}
          />,
        );
      }
      continue;
    }
    const line = member.line;
    const teamMember = teamMemberFor(line);
    if (teamMember !== null) {
      const burst = [teamMember];
      if (teamMember.operation === "start" || teamMember.operation === "known") {
        while (index + 1 < members.length) {
          const next = members[index + 1];
          if (next.kind !== "line") break;
          const candidate = teamMemberFor(next.line);
          if (candidate === null || !sameStartBurst(teamMember, candidate)) break;
          burst.push(candidate);
          index += 1;
        }
      }
      drawn.push(
        <TeamInvocation
          key={teamMember.row.timeline_id}
          members={burst}
          domIdOf={domIdOf}
          highlightedTimelineId={highlightedTimelineId}
          focusTimelineId={focusTimelineId}
        />,
      );
      continue;
    }
    const key = line.rows[0]?.timeline_id ?? line.key;
    const linkedChild = linkedChildId(line);
    drawn.push(
      <Line
        key={line.key}
        line={line}
        domId={domIdOf(key)}
        hit={
          highlightedTimelineId !== null &&
          line.rows.some((row) => row.timeline_id === highlightedTimelineId)
        }
        alive={alive(line)}
        forceOpen={inside(member)}
        subthreads={subthreadsFor(line)}
        workflowAttempts={workflowAttemptsFor(line)}
        hasChildren={hasChildren(line)}
        onOpenLinkedChild={linkedChild === null ? null : () => onOpenLinkedChild(linkedChild)}
        onOpenPath={onOpenPath}
      />
    );
  }

  // Activity gets a summary even for one line; speech has no outer fold.
  const closed = summary.closedByDefault;
  // R-5: the green breath only in a live session below the watermark, and
  // then the head carries the line that is happening.
  const running = summary.running !== null && alive(summary.running) ? summary.running : null;
  const span = summary.spanMs !== null && summary.spanMs >= 1000 ? spanText(summary.spanMs) : null;
  const expanded = override ?? (everyRunOpen || focusInside);
  const toggle = () => setOverride(!expanded);
  return (
    <div className={closed ? "tr-seg tr-ledger" : "tr-ledger"}>
      {closed ? anchor : null}
      {closed ? <div
        className="tr-line tr-fold"
        role="button"
        tabIndex={0}
        aria-expanded={expanded}
        data-state={running === null ? undefined : "running"}
        onClick={toggle}
        onKeyDown={(event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          toggle();
        }}
      >
        <span className="tr-line__g chev">
          {running === null ? <LineChevron /> : <i className="tr-dot" />}
        </span>
        <span className="tr-line__v">
          {summary.nouns.map((noun, index) => (
            <Fragment key={noun.family}>
              {index > 0 && " · "}
              {noun.text}
              {noun.names.length > 0 && (
                <>
                  {" "}
                  <span className="names">
                    {noun.names.join(", ")}
                    {noun.more > 0 ? ` … ${noun.more} more` : ""}
                  </span>
                </>
              )}
            </Fragment>
          ))}
          {running !== null && (
            <span className="cur">
              {summary.nouns.length > 0 && " · "}
              {running.verb} <span className="tr-line__o">{objectText(running.object)}</span>
            </span>
          )}
        </span>
        <span className="tr-line__r">
          {summary.failed > 0 && <b>{summary.failed} failed</b>}
          {summary.errors > 0 && (
            <b>
              {summary.errors} {summary.errors === 1 ? "error" : "errors"}
            </b>
          )}
          {span !== null && (
            <span className={running === null ? undefined : "why"}>
              {running === null ? span : `${span} → …`}
            </span>
          )}
        </span>
      </div> : null}
      {/* One wrapper in both states keeps each line's open state (R-5).
          Always in the DOM, shown by the head's own `aria-expanded`. */}
      <div className={closed ? "tr-ladder" : undefined}>{drawn}</div>
    </div>
  );
}
