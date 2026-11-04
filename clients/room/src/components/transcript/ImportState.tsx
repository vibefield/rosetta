// What a transcript says while the import has not reached it.
//
// A session still Pending says when it opens, only as far as its runtime's
// course promises (`ImportCourse`), never with an order or a position; a row
// the metadata pass made only for its children says its transcript was never
// found; an open one says its live updates wait for the import to finish.
// The catalog and the session socket announce a tree's opening separately,
// so until rows delivered after the flip replace the empty Pending ones, the
// transcript keeps its not-imported face (`watchImport`): it never reads
// "Nothing readable yet" in between.
import { useEffect, useState } from "react";
import type { SessionRow } from "@vibefield/rosetta-protocol";
import type { TimelineFeed } from "../../lib/hooks";
import { NOT_IMPORTED_LABEL, holdsImport, watchImport } from "../../lib/import-state";
import type { ImportCourse, ImportStanding, ImportWatch } from "../../lib/import-state";
import { EmptyState, Meter } from "../ui";

/** Why a session is not open yet, and when it will be: each promise only as
 * far as the import's course can keep it (`ImportCourse`). A tree of a
 * per-tree runtime may still wait for the runtime's close (bound), so that
 * course promises no time. */
function notImportedWhy(course: ImportCourse, agent: string | undefined): string {
  switch (course) {
    case "newest-first": return "Newer conversations import first. This one opens here once it is imported.";
    case "runtime-close": return `This one opens when ${agent ?? "its agent"} finishes importing.`;
    case "resumed": return "The import picked up where it stopped. This one opens when the import finishes.";
    case "stopped": return "The import stopped before it reached this one. It opens once the import continues.";
  }
}

/** Said once, politely, when the open session's import face gives way to
 * its rows. */
export const IMPORTED_ANNOUNCEMENT = "Conversation imported";

/** How long the not-imported face waits for the resync a flip promises
 * before it asks the session again. The daemon resyncs a watched session
 * once at its flip; this covers a subscription that raced it. */
export const IMPORT_RESYNC_GRACE_MS = 3_000;

/** True while the session has opened but its rows are still the empty ones
 * delivered while it was Pending. */
export function useImportHold(session: SessionRow, feed: TimelineFeed, enabled: boolean): boolean {
  // Rows keep their identity until the feed delivers again, but only once
  // its snapshot is ready: before that, nothing has been delivered.
  const delivered = feed.snapshotReady ? feed.rows : null;
  const [watch, setWatch] = useState<ImportWatch | null>(null);
  const next = watchImport(watch, session.id, session.stream_state, delivered);
  if (next !== watch) setWatch(next);
  const holding = enabled && holdsImport(next, session.stream_state);
  const { retry } = feed;
  useEffect(() => {
    if (!holding) return undefined;
    const timer = setTimeout(retry, IMPORT_RESYNC_GRACE_MS);
    return () => clearTimeout(timer);
  }, [holding, retry]);
  return holding;
}

/** True once the session showed its not-imported face and then its rows:
 * the transcript's one announcement that it opened (the hold and the reveal
 * themselves are `useImportHold`'s). */
export function useImportedAnnouncement(waiting: boolean, readable: boolean): boolean {
  const [seen, setSeen] = useState({ waited: false, opened: false });
  const next = waiting ? (seen.waited ? seen : { waited: true, opened: false })
    : seen.waited && readable && !seen.opened ? { waited: true, opened: true } : seen;
  if (next !== seen) setSeen(next);
  return next.opened;
}

/** The polite live region a transcript keeps, empty until its session opens
 * under the reader. `data-import-announce` is inert, for the DOM contract. */
export function ImportAnnouncement({ opened }: { readonly opened: boolean }) {
  return (
    <div className="ah-visually-hidden" role="status" aria-live="polite" data-import-announce={opened ? "imported" : ""}>
      {opened ? IMPORTED_ANNOUNCEMENT : ""}
    </div>
  );
}

/** The empty transcript of a session the import has not reached, or never will. */
export function ImportEmptyState({ standing, duringBackfill, course = "newest-first", agent }: {
  readonly standing: Exclude<ImportStanding, null>;
  readonly duringBackfill: boolean;
  readonly course?: ImportCourse | undefined;
  /** The session's agent, named by a `runtime-close` promise. */
  readonly agent?: string | undefined;
}) {
  // `data-import-face` and `data-import-course` are inert: the face and the
  // course its words come from, for the DOM contract.
  if (standing === "unavailable") return (
    <EmptyState
      title="Transcript not available"
      description="Its own transcript was not found, only the conversations it started."
      data-import-face="unavailable"
    />
  );
  return (
    <EmptyState title={NOT_IMPORTED_LABEL} description={notImportedWhy(course, agent)}
      data-import-face="not-imported" data-import-course={course}>
      {/* No amount is known for one session: the meter sweeps, unless the
          import is stopped and nothing moves. */}
      {duringBackfill && course !== "stopped" && <Meter fraction={null} />}
    </EmptyState>
  );
}

/** Above the conversations a metadata-only row started, which are all it has. */
export function TranscriptUnavailableMark() {
  return (
    <div className="tr-mark" role="note" data-transcript-unavailable="">
      Transcript not available · only the conversations it started
    </div>
  );
}

/** Above an open transcript while the import runs: its rows are what the
 * import read, and the live watcher starts only once it finishes. */
export function ImportBanner() {
  return (
    <div className="tr-mark" role="note" data-import-banner="">
      Live updates resume when the import finishes
    </div>
  );
}
