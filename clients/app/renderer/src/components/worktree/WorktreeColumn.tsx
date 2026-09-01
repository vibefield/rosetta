// Column ③ — the project's repository: its files, what git has not recorded
// yet, and what it has.
//
// The design record draws these as two columns, a worktree beside a git
// panel, each opening from a different head. They are one column here,
// because they are three views of ONE object: this project's checkout. Two
// columns made the reader hold that identity in their head — the same folder
// named twice, in two heads, with two toggles — and cost 280px to say it. A
// switch says it in a row the column already had.
//
// It is still a guest (H-3), and now the only one, so the record's original
// single-subject rule applies again: below a 1360px frame it closes rather
// than squeeze the transcript.

import { useCallback, useEffect, useMemo, useState } from "react";
import type { Entry } from "@vibecook/mille";
import { desktopHost } from "../../lib/desktop-host";
import { FileViewerDialog } from "../file-viewer";
import type { FileHandle } from "../file-viewer";
import { ConfirmDialog, EmptyState, SegmentedControl } from "../ui";
import {
  changeShapeOf,
  CommitViewer,
  createGitFileSource,
  diffHandle,
  GitChanges,
  GitHistory,
  useGitRead,
} from "../git";
import type { Commit, CommitFile, GitClient } from "../git";
import { useGitFacts, useWorktreeConnection } from "./worktree-host";
import { createWorktreeFileSource, handleFor, isOpenable } from "./worktree-file-source";
import { WorktreeFileTree } from "./WorktreeFileTree";
import { WorktreeHeader } from "./WorktreeHeader";
import { WorktreeList } from "./WorktreeList";
import type { RepoTab } from "./types";

export interface WorktreeColumnProps {
  /** The selected project's first root path, from rosettad. */
  readonly projectRoot: string | null;
  /** The project's §2.6 accent, following it across the room. */
  readonly tint: string | null;
  readonly homeDirectory: string | null;
  /** Paths the open session touched, workspace-relative. */
  readonly sessionPaths: ReadonlySet<string>;
  readonly currentTimestamp: number;
  /** The room's receipt line, so an act is never silent. */
  readonly onReceipt: (text: string) => void;
  readonly requestError: string | null;
  readonly onRetry: () => void;
}

const TABS = [
  { value: "files", label: "Files" },
  { value: "changes", label: "Changes" },
  { value: "history", label: "History" },
] as const;

const readBranch = (client: GitClient) => client.branch();
const readStatus = (client: GitClient) => client.status();
const readLog = (client: GitClient) => client.log();

/** What the dialog currently holds. A file read and a diff are two different
 * acts on two different sources, and the handle alone cannot say which. */
interface OpenTarget {
  readonly handle: FileHandle;
  readonly diff: boolean;
}

export function WorktreeColumn({
  projectRoot,
  tint,
  homeDirectory,
  sessionPaths,
  currentTimestamp,
  onReceipt,
  requestError,
  onRetry,
}: WorktreeColumnProps) {
  const lifecycle = useWorktreeConnection();
  const live = lifecycle.status === "ready" ? lifecycle.connection : null;
  const engine = live?.engine ?? null;
  // The engine follows the selection with a lag (kill, re-fork, walk), so the
  // head names what the column is actually showing, never what was asked for.
  const shownRoot =
    lifecycle.status === "ready"
      ? lifecycle.connection.root
      : lifecycle.status === "idle"
        ? projectRoot
        : lifecycle.root;
  const facts = useGitFacts(live?.root ?? null, live?.generation ?? null);
  const connection = live?.git ?? null;
  const client = connection?.client ?? null;

  const [tab, setTab] = useState<RepoTab>("files");
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
    () =>
      commits.some((commit) => commit.pushed === null)
        ? null
        : commits.filter((commit) => commit.pushed === false).length,
    [commits],
  );

  const hasShell = desktopHost() !== null;
  const stale = projectRoot !== null && shownRoot !== null && shownRoot !== projectRoot;
  const hasSiblingWorktrees = facts !== null && facts.worktrees.length > 1;
  const lifecycleError =
    lifecycle.status === "error" && lifecycle.root === projectRoot
      ? lifecycle.message
      : requestError;

  const [open, setOpen] = useState<OpenTarget | null>(null);
  const [selection, setSelection] = useState<{ path: string; staged: boolean } | null>(null);
  const [openCommit, setOpenCommit] = useState<Commit | null>(null);
  const [commitFiles, setCommitFiles] = useState<readonly CommitFile[] | null>(null);
  const [pendingDiscard, setPendingDiscard] = useState<readonly string[] | null>(null);

  // One source per engine and one per client: the read hook treats a new
  // source as a new request, and a project switch invalidates every id and
  // every path the old ones issued.
  const fileSource = useMemo(
    () => (engine === null ? null : createWorktreeFileSource(engine)),
    [engine],
  );
  const gitSource = useMemo(() => (client === null ? null : createGitFileSource(client)), [client]);
  useEffect(() => {
    setOpen(null);
    setSelection(null);
    setOpenCommit(null);
  }, [fileSource, gitSource]);

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
    setOpen({
      handle: diffHandle({ path, staged }, { root: connection.root, label: path }),
      diff: true,
    });
  };

  const showCommit = (commit: Commit) => {
    setOpenCommit(commit);
    setCommitFiles(null);
    setSelection(null);
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

  const count =
    tab === "history"
      ? commits.length
      : tab === "changes"
        ? entries.length
        : // The Files tab has no honest count: we know what the tree is
          // showing, not what the repository holds.
          null;

  const source = open?.diff === true ? gitSource : fileSource;

  return (
    <div className="ah-col ah-col--rail">
      <WorktreeHeader
        root={shownRoot}
        tint={tint}
        homeDirectory={homeDirectory}
        facts={facts}
        branch={branch}
        tab={tab}
        count={count}
        changeShape={changeShapeOf(entries)}
        unpushed={unpushed}
      />

      <div className="ah-listbar">
        <SegmentedControl
          className="ah-gitseg"
          accessibleLabel="What this column shows"
          value={tab}
          options={TABS.map((option) => ({ value: option.value, label: option.label }))}
          onValueChange={(next) => {
            setTab(next as RepoTab);
            setSelection(null);
          }}
        />
      </div>

      {!hasShell ? (
        <EmptyState
          title="No repository here"
          description="This column needs the desktop shell — a browser tab has no worktree to read."
        />
      ) : projectRoot === null ? (
        <EmptyState
          title="No project selected"
          description="Pick a project to browse its worktree."
        />
      ) : lifecycleError !== null ? (
        <EmptyState title="Worktree unavailable" description={lifecycleError}>
          <button
            type="button"
            onClick={onRetry}
            style={{
              border: "1px solid var(--vf-border)",
              borderRadius: 6,
              padding: "4px 9px",
              color: "var(--vf-text-secondary)",
              background: "var(--vf-surface-raised)",
              cursor: "pointer",
            }}
          >
            Try again
          </button>
        </EmptyState>
      ) : tab === "files" ? (
        engine === null || fileSource === null ? (
          <EmptyState
            title="Opening the worktree"
            description="The engine is walking the project root."
          />
        ) : (
          <>
            <WorktreeFileTree
              engine={engine}
              stale={stale}
              openEntryId={
                open?.diff === false && typeof open.handle.ref === "number" ? open.handle.ref : null
              }
              onOpen={(entry: Entry) => {
                // A folder toggles rather than opens; mille only reports rows
                // with no children, which an empty directory also is.
                if (isOpenable(entry)) setOpen({ handle: handleFor(engine, entry), diff: false });
              }}
            />
            {hasSiblingWorktrees && (
              <WorktreeList facts={facts} openRoot={shownRoot} homeDirectory={homeDirectory} />
            )}
          </>
        )
      ) : connection === null ? (
        <EmptyState title="Reading the repository" description="Asking git for its status." />
      ) : tab === "changes" ? (
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
          selectedSha={openCommit}
          onSelect={showCommit}
          currentTimestamp={currentTimestamp}
        />
      )}

      {openCommit !== null && (
        <CommitViewer
          commit={openCommit}
          files={commitFiles}
          client={client}
          sessionPaths={sessionPaths}
          currentTimestamp={currentTimestamp}
          onReceipt={onReceipt}
          onClose={() => {
            setOpenCommit(null);
            setCommitFiles(null);
          }}
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
          handle={open?.handle ?? null}
          source={source}
          tint={tint}
          homeDirectory={homeDirectory}
          onClose={() => setOpen(null)}
          onReceipt={onReceipt}
          openInBrowser={open?.diff === true ? null : (desktopHost()?.openInBrowser ?? null)}
          // The Files tab opens a FILE; the other two open a CHANGE. The
          // registry reads kinds off a name, and a name cannot say which.
          asDiff={open?.diff === true}
        />
      )}
    </div>
  );
}
