// One commit, opened.
//
// The column changes what it holds, never how big it is (H-2): drilling into
// a commit replaces the list with that commit's files rather than growing a
// pane or opening a second surface. The file list is the same subtree the
// Changes view draws, for the same reason — a path a reader learned in one
// view still holds in the other.

import { useMemo } from "react";
import { clockShort, dayLabel } from "../../lib/format";
import { EmptyState } from "../ui";
import { ChangeTree } from "./ChangeTree";
import { buildPathTree } from "./path-tree";
import type { Commit, CommitFile, StatusLetter } from "./types";

export interface CommitDetailProps {
  readonly commit: Commit;
  readonly currentTimestamp: number;
  /** `null` until the files land; a commit always has some. */
  readonly files: readonly CommitFile[] | null;
  readonly selectedPath: string | null;
  readonly onSelect: (path: string) => void;
  readonly sessionPaths: ReadonlySet<string>;
}

export function CommitDetail({
  commit,
  currentTimestamp,
  files,
  selectedPath,
  onSelect,
  sessionPaths,
}: CommitDetailProps) {
  const rows = useMemo(
    () => buildPathTree((files ?? []).map((file) => file.path)),
    [files],
  );
  const letters = useMemo(() => {
    const out = new Map<string, StatusLetter>();
    for (const file of files ?? []) out.set(file.path, file.status);
    return out;
  }, [files]);

  if (files === null) {
    return <EmptyState title="Reading the commit" description="Asking git what it touched." />;
  }
  if (files.length === 0) {
    return (
      <EmptyState
        title="This commit touched no files"
        description={commit.merge ? "It is a merge, and its parents carry the work." : "An empty commit."}
      />
    );
  }

  return (
    <div className="ah-scroll">
      {/* The message in full, because the row above it had to truncate and
          this is the surface that does not — and with it everyone git says
          wrote it, which is the one place a co-author is not competing with
          a diffstat for room. */}
      <p className="ah-commitmsg">{commit.subject}</p>
      <p className="ah-commitwho">
        {[commit.author, ...commit.coAuthors.map((author) => author.name)]
          .filter((name) => name !== "")
          .map((name, index) => (
            <span key={name} data-co={index > 0 ? "" : undefined}>
              {name}
            </span>
          ))}
        {/* `Today 15:04`, or `Mon 24 Aug 15:04` for an older one. The row
            this came from gives the relative age; the surface you opened
            gives the date, because "4h" stops being an answer at some point
            and a commit list goes back years. */}
        <time dateTime={new Date(commit.timestampMs).toISOString()}>
          {dayLabel(commit.timestampMs, currentTimestamp)} {clockShort(commit.timestampMs)}
        </time>
      </p>
      <div className="ah-group">
        Files
        <span className="ah-hitcount tnum">{files.length}</span>
      </div>
      <ChangeTree
        rows={rows}
        letters={letters}
        sessionPaths={sessionPaths}
        selectedPath={selectedPath}
        onSelect={(row) => row.kind === "file" && onSelect(row.path)}
        // A commit is history. Nothing here stages, unstages or discards —
        // those act on the working tree, and this is not it.
        action={null}
        onDiscard={null}
      />
    </div>
  );
}
