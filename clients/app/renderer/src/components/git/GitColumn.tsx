// Column ⑤ — the repository's git, opened from the head of the transcript
// and sitting immediately to its right, because a guest opens from the head
// of the column it describes (design record §8).
//
// Scoped to the REPOSITORY, with the files this session touched wearing a
// mark. The record offered the opposite — a column scoped to one session,
// its Q4 "keep or kill" — and this keeps the idea while dropping the filter:
// a list that hides the other twenty changed files is not one you can act
// from, and the session attribution survives as the thing no other git client
// can draw.

import { useCallback, useMemo, useState } from "react";
import { FileViewerDialog } from "../file-viewer";
import type { FileHandle } from "../file-viewer";
import { ConfirmDialog, EmptyState, SegmentedControl } from "../ui";
import { useGitClient, useGitRead } from "./git-host";
import { createGitFileSource, diffHandle } from "./git-file-source";
import { CommitDetail } from "./CommitDetail";
import { GitChanges } from "./GitChanges";
import { GitHeader } from "./GitHeader";
import { GitHistory } from "./GitHistory";
import type { Commit, CommitFile, GitClient, GitView } from "./types";

export interface GitColumnProps {
  /** The project's §2.6 accent, following it across the room. */
  readonly tint: string | null;
  readonly homeDirectory: string | null;
  /** Paths the open session touched, workspace-relative. */
  readonly sessionPaths: ReadonlySet<string>;
  readonly currentTimestamp: number;
  readonly onReceipt: (text: string) => void;
}

const readBranch = (client: GitClient) => client.branch();
const readStatus = (client: GitClient) => client.status();
const readLog = (client: GitClient) => client.log();

/** `2 added · 5 modified · 1 deleted` — what git would say about this set in
 * one line, counted off the entries the column is drawing. */
function changeShape(entries: readonly { index: string | null; worktree: string | null }[]): string {
  const tally = { added: 0, modified: 0, deleted: 0, untracked: 0, conflicted: 0 };
  for (const entry of entries) {
    // One file, one word: the index's letter is what a commit would record,
    // so it speaks first.
    const letter = entry.index ?? entry.worktree;
    if (letter === "A") tally.added += 1;
    else if (letter === "D") tally.deleted += 1;
    else if (letter === "?") tally.untracked += 1;
    else if (letter === "U") tally.conflicted += 1;
    else if (letter !== null) tally.modified += 1;
  }
  return (
    [
      [tally.conflicted, "conflicted"],
      [tally.added, "added"],
      [tally.modified, "modified"],
      [tally.deleted, "deleted"],
      [tally.untracked, "untracked"],
    ] as const
  )
    .filter(([count]) => count > 0)
    .map(([count, word]) => `${count} ${word}`)
    .join(" · ");
}

export function GitColumn({
  tint,
  homeDirectory,
  sessionPaths,
  currentTimestamp,
  onReceipt,
}: GitColumnProps) {
  const connection = useGitClient();
  const client = connection?.client ?? null;
  const [view, setView] = useState<GitView>("changes");
  // Bumped after a write so the next read is immediate rather than waiting
  // out the poll — a stage that takes four seconds to show reads as a stage
  // that did not happen.
  const [revision, setRevision] = useState(0);

  const branch = useGitRead(connection, readBranch, revision).value;
  const status = useGitRead(connection, readStatus, revision).value;
  const log = useGitRead(connection, readLog, revision).value;

  const entries = status?.entries ?? [];
  const commits = log?.commits ?? [];
  const unpushed = useMemo(
    () => (commits.some((c) => c.pushed === null) ? null : commits.filter((c) => c.pushed === false).length),
    [commits],
  );

  const source = useMemo(() => (client === null ? null : createGitFileSource(client)), [client]);
  const [open, setOpen] = useState<FileHandle | null>(null);
  const [selection, setSelection] = useState<{ path: string; staged: boolean } | null>(null);
  const [selectedSha, setSelectedSha] = useState<string | null>(null);
  // The commit the column has drilled into, and its files. Held here rather
  // than inside the detail view so returning to the list and opening the
  // same commit again does not re-ask git for what it just answered.
  const [openCommit, setOpenCommit] = useState<Commit | null>(null);
  const [commitFiles, setCommitFiles] = useState<readonly CommitFile[] | null>(null);
  // Discard is the one act in this room that destroys something. It asks.
  const [pendingDiscard, setPendingDiscard] = useState<readonly string[] | null>(null);

  /** Run a write, then read straight back. The receipt names what happened,
   * because a git act that reports nothing is one you check by hand. */
  const act = useCallback(
    (label: string, paths: readonly string[], run: (paths: readonly string[]) => Promise<void>) => {
      void run(paths)
        .then(() => {
          onReceipt(`${label} ${paths.length} ${paths.length === 1 ? "file" : "files"}`);
          setRevision((n) => n + 1);
        })
        .catch((error: unknown) => {
          onReceipt(error instanceof Error ? error.message : `could not ${label.toLowerCase()}`);
          setRevision((n) => n + 1);
        });
    },
    [onReceipt],
  );

  const showDiff = (path: string, staged: boolean) => {
    if (connection === null) return;
    setSelection({ path, staged });
    setSelectedSha(null);
    setOpen(diffHandle({ path, staged }, { root: connection.root, label: path }));
  };

  const showCommit = (commit: Commit) => {
    setSelectedSha(commit.sha);
    setOpenCommit(commit);
    setCommitFiles(null);
    if (client === null) return;
    void client
      .commitFiles(commit.sha)
      .then((answer) => {
        if (answer.error !== null) {
          onReceipt(answer.error);
          setCommitFiles([]);
          return;
        }
        setCommitFiles(answer.files);
      })
      .catch((error: unknown) => {
        onReceipt(error instanceof Error ? error.message : "could not read that commit");
        setCommitFiles([]);
      });
  };

  const leaveCommit = () => {
    setOpenCommit(null);
    setCommitFiles(null);
    setSelection(null);
  };

  /** A file inside the open commit, diffed against its parent. */
  const showCommitFile = (path: string) => {
    if (connection === null || openCommit === null) return;
    setSelection({ path, staged: false });
    setOpen(diffHandle({ path, sha: openCommit.sha }, { root: connection.root, label: path }));
  };

  const count =
    openCommit !== null
      ? (commitFiles?.length ?? openCommit.files)
      : view === "history"
        ? commits.length
        : entries.length;

  return (
    <div className="ah-col ah-col--foot">
      <GitHeader
        branch={branch}
        tint={tint}
        view={view}
        count={count}
        changeShape={changeShape(entries)}
        unpushed={unpushed}
        commit={openCommit}
        onBack={leaveCommit}
      />
      {/* The switch belongs to the two views of a branch. Inside a commit
          there is nothing to switch between, and leaving it there would
          offer a control that changes the surface out from under you. */}
      {openCommit === null && (
        <div className="ah-listbar">
          <SegmentedControl
            className="ah-gitseg"
            accessibleLabel="What this column shows"
            value={view}
            options={[
              { value: "changes", label: "Changes" },
              { value: "history", label: "History" },
            ]}
            onValueChange={(next) => {
              setView(next as GitView);
              setSelection(null);
            }}
          />
        </div>
      )}

      {connection === null ? (
        <EmptyState
          title="No git here"
          description="The column needs the desktop shell — a browser tab has no repository to read."
        />
      ) : status === null && log === null ? (
        <EmptyState title="Reading the repository" description="Asking git for its status." />
      ) : openCommit !== null ? (
        <CommitDetail
          commit={openCommit}
          currentTimestamp={currentTimestamp}
          files={commitFiles}
          selectedPath={selection?.path ?? null}
          onSelect={showCommitFile}
          sessionPaths={sessionPaths}
        />
      ) : view === "changes" ? (
        <GitChanges
          entries={entries}
          sessionPaths={sessionPaths}
          selection={selection}
          onSelect={showDiff}
          onStage={client === null ? null : (paths) => act("Staged", paths, client.stage)}
          onUnstage={client === null ? null : (paths) => act("Unstaged", paths, client.unstage)}
          onDiscard={client === null ? null : setPendingDiscard}
        />
      ) : (
        <GitHistory
          commits={commits}
          selectedSha={selectedSha}
          onSelect={showCommit}
          currentTimestamp={currentTimestamp}
        />
      )}

      <ConfirmDialog
        question={
          pendingDiscard === null
            ? null
            : {
                title:
                  pendingDiscard.length === 1
                    ? "Discard this file's changes?"
                    : `Discard changes to ${pendingDiscard.length} files?`,
                body: (
                  <>
                    <p>
                      This throws the work away. It is not staged and it is not committed, so
                      git has no copy of it and neither does this room.
                    </p>
                    <ul className="ah-confirm__list">
                      {pendingDiscard.slice(0, 8).map((path) => (
                        <li key={path}>{path}</li>
                      ))}
                      {pendingDiscard.length > 8 && (
                        <li data-more>and {pendingDiscard.length - 8} more</li>
                      )}
                    </ul>
                  </>
                ),
                confirm: "Discard",
                destructive: true,
              }
        }
        onCancel={() => setPendingDiscard(null)}
        onConfirm={() => {
          if (pendingDiscard !== null && client !== null) {
            act("Discarded", pendingDiscard, client.discard);
          }
          setPendingDiscard(null);
        }}
      />

      {source !== null && (
        <FileViewerDialog
          handle={open}
          source={source}
          tint={tint}
          homeDirectory={homeDirectory}
          onClose={() => setOpen(null)}
          onReceipt={onReceipt}
          openInBrowser={null}
          // Everything this column opens is a CHANGE. The registry reads
          // kinds off a name, and `main.rs` is a source file to the tree and
          // a diff here — only the caller knows which.
          asDiff
        />
      )}
    </div>
  );
}
