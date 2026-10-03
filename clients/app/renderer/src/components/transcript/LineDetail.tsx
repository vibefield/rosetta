// §04's last column — what a ledger line opens into.
//
// One rule governs every branch: the detail shows what the ROW proved and
// nothing else. A read keeps no content in the projection, so it opens into
// its targets; an explicit target press may ask the daemon for the live file.
// A fetch opens into its final url, title and status and never the body; a
// change without a stored diff says `no diff recorded` rather than drawing an
// empty one. Absence is stated, never faked (§06).
//
// Q5 A — the diff opens IN PLACE, capped at twelve lines. The record accepted
// two renderers of one diff for it: the file viewer's `DiffBody` is virtual,
// full-height and lives in a dialog, and the twelve lines under a 26px ledger
// line are a different object. The PARSER is shared (`file-viewer/diff`), so
// the two renderers can never disagree about what git said.
import type { ReactNode } from "react";
import type { LedgerDetail, PathFact } from "../../lib/transcript";
import { resourceDisplay } from "../../lib/transcript";
import type { SourceLocation } from "../../lib/transcript/protocol";
import {
  CommandList,
  DiffDetail,
  Document,
  Fields,
  Findings,
  Participants,
  PathList,
  Questions,
  Tasks,
  LIST_CAP,
} from "./detail-parts";
import { SourceList, FetchDetails } from "./WebDetail";
import { WorkflowDetail } from "./Workflow";
import type { WorkflowChildThread } from "./Workflow";

function LocationList({
  locations,
  total,
}: {
  readonly locations: readonly SourceLocation[];
  readonly total: number;
}) {
  const shown = locations.slice(0, LIST_CAP);
  return (
    <ul className="tr-paths">
      {shown.map((location, at) => (
        <li key={at}>
          {resourceDisplay(location.resource, {})}
          {location.line !== null && `:${location.line}`}
          {location.summary !== null && <span className="n">{location.summary}</span>}
        </li>
      ))}
      {total > shown.length && <li className="tr-more">… {total - shown.length} more</li>}
    </ul>
  );
}

export interface LineDetailProps {
  readonly detail: LedgerDetail;
  /** The bounded failure text. Never the tool's output. */
  readonly diagnostic: string | null;
  /** The delegate's child run, resolved by the document and passed in — this
   * module knows the shape of a detail, not the shape of a session tree. */
  readonly subthread?: ReactNode;
  /** Workflow children stay attached to their own worker row instead of
   * becoming an undifferentiated stack below the run document. */
  readonly workflowChildren?: readonly WorkflowChildThread[];
  readonly workflowAttempts?: readonly import("../../lib/transcript").WorkflowAttemptFact[];
  /** The document owns the viewer; detail rows only report which exact path
   * the reader pressed. */
  readonly onOpenPath?: ((path: PathFact) => void) | undefined;
}

/** The body of one open line. Returns null when the family has nothing honest
 * to show, which is what makes the line unpressable in `Ledger`. */
export function LineDetail({
  detail,
  diagnostic,
  subthread,
  workflowChildren = [],
  workflowAttempts = [],
  onOpenPath,
}: LineDetailProps) {
  const body = ((): ReactNode => {
    switch (detail.kind) {
      case "none":
        return null;
      case "paths":
        return (
          <>
            {detail.description !== undefined && <p className="tr-text">{detail.description}</p>}
            <PathList paths={detail.paths} total={detail.total} onOpenPath={onOpenPath} />
          </>
        );
      case "locations":
        return <LocationList locations={detail.locations} total={detail.total} />;
      case "sources":
        return <SourceList sources={detail.sources} total={detail.total} />;
      case "fetch":
        return <FetchDetails fetches={[detail.fetch]} />;
      case "fetches":
        return <FetchDetails fetches={detail.fetches} />;
      case "diff":
        return <DiffDetail deltas={detail.deltas} diffstat={detail.diffstat} />;
      case "command":
        return <CommandList commands={detail.commands} code={false} />;
      case "code":
        return <CommandList commands={detail.commands} code />;
      case "names":
        return (
          <ul className="tr-paths">
            {detail.names.slice(0, LIST_CAP).map((name, at) => (
              <li key={at}>{name}</li>
            ))}
            {detail.total > Math.min(detail.names.length, LIST_CAP) && (
              <li className="tr-more">
                … {detail.total - Math.min(detail.names.length, LIST_CAP)} more
              </li>
            )}
          </ul>
        );
      case "tasks":
        return (
          <Tasks
            tasks={detail.tasks}
            total={detail.total}
            changedTaskId={detail.changedTaskId}
            change={detail.change}
          />
        );
      case "text":
        return <p className="tr-text">{detail.text}</p>;
      case "questions":
        return <Questions questions={detail.questions} answers={detail.answers} />;
      case "findings":
        return <Findings findings={detail.findings} total={detail.total} />;
      case "document":
        return <Document document={detail.document} />;
      case "fields":
        return (
          <>
            <Fields fields={detail.fields} />
            {detail.resources.length > 0 && (
              <PathList paths={detail.resources} total={detail.resources.length} />
            )}
          </>
        );
      case "delegate":
        return detail.delegate.orphanSummary === null ? null : (
          // The child was never indexed, so the record draws its result
          // summary as prose rather than an empty subthread.
          <p className="tr-text">{detail.delegate.orphanSummary}</p>
        );
      case "workflow":
        return (
          <WorkflowDetail
            fact={detail.workflow}
            attempts={workflowAttempts}
            children={workflowChildren}
            onOpenPath={onOpenPath}
          />
        );
      case "participants":
        return (
          <Participants
            participants={detail.participants}
            total={detail.total}
            body={detail.body}
          />
        );
      case "diagnostic":
        return <pre className="tr-diag">{detail.diagnostic}</pre>;
    }
  })();

  // A failure always opens to its bounded diagnostic, even when the family
  // already had something to say (§04: shell opens into the command, its cwd,
  // AND on failure the diagnostic).
  const extra =
    diagnostic !== null && detail.kind !== "diagnostic" ? (
      <pre className="tr-diag">{diagnostic}</pre>
    ) : null;

  if (body === null && extra === null && subthread === undefined) return null;
  return (
    <>
      {body}
      {extra}
      {subthread}
    </>
  );
}

/** Whether pressing the line has anything to open. Keeps the DOM honest: a
 * line that opens into nothing is not focusable and does not take a press. */
export function hasDetail(detail: LedgerDetail, diagnostic: string | null): boolean {
  if (diagnostic !== null) return true;
  switch (detail.kind) {
    case "none":
      return false;
    case "delegate":
      return detail.delegate.childSessionId !== null || detail.delegate.orphanSummary !== null;
    default:
      return true;
  }
}
