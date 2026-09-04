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
//   T-3  past eight lines the whole run closes to ONE sentence of nouns with
//        the failure count in red and the span at the right. The ladder stays
//        in the DOM: the column head's activity act reveals every run at once
//        through CSS alone (`[data-fold="off"]`), so a room-wide preference
//        never has to walk React state.
//   T-5  hue only for what did not complete or is still happening. The green
//        breath is drawn ONLY in a live session below the watermark — a
//        `running` row in a finished transcript is a record that stopped, and
//        it reads muted rather than pretending to be alive.
import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import { clock } from "../../lib/format";
import type {
  LedgerLine,
  ObjectPart,
  PathFact,
  RunSummary,
  TeamMemberPresentation,
  WorkflowAttemptFact,
} from "../../lib/transcript";
import { objectText, spanText } from "../../lib/transcript";
import { FaviconStack, Favicon } from "./Favicon";
import { FailGlyph, FamilyGlyph, LineChevron } from "./glyphs";
import { hasDetail, LineDetail } from "./LineDetail";
import { TeamInvocation } from "./Team";
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

  return (
    <>
      <div
        className="tr-line"
        id={domId}
        data-timeline-id={line.rows[0]?.timeline_id}
        data-state={state}
        data-hit={hit || undefined}
        role={pressable ? "button" : undefined}
        tabIndex={pressable ? 0 : undefined}
        aria-expanded={!navigatesToChild && pressable ? isOpen : undefined}
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
              <span className="ttl">{fetched.title}</span>
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
              <div className="tr-detail">{body}</div>
            ) : null;
          })()}
          {line.detail.kind === "workflow" ? null : subthreads.map((thread) => thread.node)}
        </>
      )}
    </>
  );
}

export interface LedgerProps {
  readonly lines: readonly LedgerLine[];
  readonly summary: RunSummary;
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

export function Ledger({
  lines,
  summary,
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
  const focusInside = lines.some(
    (line) =>
      childFocused(line) ||
      (focusTimelineId !== null &&
        line.rows.some((row) => row.timeline_id === focusTimelineId)),
  );
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (focusInside) setOpen(true);
  }, [focusInside]);

  const drawn: ReactNode[] = [];
  for (let at = 0; at < lines.length; at += 1) {
    const line = lines[at];
    const teamMember = teamMemberFor(line);
    if (teamMember !== null) {
      const members = [teamMember];
      if (teamMember.operation === "start" || teamMember.operation === "known") {
        while (at + 1 < lines.length) {
          const candidate = teamMemberFor(lines[at + 1]);
          if (candidate === null || !sameStartBurst(teamMember, candidate)) break;
          members.push(candidate);
          at += 1;
        }
      }
      drawn.push(
        <TeamInvocation
          key={teamMember.row.timeline_id}
          members={members}
          domIdOf={domIdOf}
          highlightedTimelineId={highlightedTimelineId}
          focusTimelineId={focusTimelineId}
        />,
      );
      continue;
    }
    const key = line.rows[0]?.timeline_id ?? line.key;
    const inside =
      childFocused(line) ||
      (focusTimelineId !== null &&
        line.rows.some((row) => row.timeline_id === focusTimelineId));
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
        forceOpen={inside}
        subthreads={subthreadsFor(line)}
        workflowAttempts={workflowAttemptsFor(line)}
        hasChildren={hasChildren(line)}
        onOpenLinkedChild={linkedChild === null ? null : () => onOpenLinkedChild(linkedChild)}
        onOpenPath={onOpenPath}
      />
    );
  }

  if (!summary.closedByDefault) return <div className="tr-ledger">{drawn}</div>;

  const span = summary.spanMs !== null && summary.spanMs >= 1000 ? spanText(summary.spanMs) : null;
  const expanded = open || focusInside;
  return (
    <div className="tr-ledger">
      <div
        className="tr-line tr-fold"
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
        <span className="tr-line__g chev">
          <LineChevron />
        </span>
        <span className="tr-line__v">
          {summary.nouns.join(" · ")}
          {summary.failed > 0 && (
            <>
              {" · "}
              <b>{summary.failed} failed</b>
            </>
          )}
        </span>
        <span className="tr-line__r">{span !== null && <span>{span}</span>}</span>
      </div>
      {/* Always in the DOM: `[data-fold="off"]` reveals every ladder at once,
          which is the column head's activity act — one CSS switch, no walk. */}
      <div className="tr-ladder">{drawn}</div>
    </div>
  );
}
