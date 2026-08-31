// One commit, opened over the room.
//
// This is the shape a commit actually wants and a 280px column could never
// give it: the files on the left, the change on the right, both at once. It
// is the same surface the file viewer uses — same shell, same material, same
// Escape — because a reader opening a commit and a reader opening a file are
// doing the same kind of thing, and the room should not have two answers for
// "something opened over what I was reading".
//
// The left pane is the Changes view's own subtree, so a path learned in the
// column still holds here; the right pane is the file viewer's own diff, so
// a change looks the same wherever it is read.

import { useEffect, useMemo, useState } from "react";
import { DiffBody } from "../file-viewer";
import { AgentMark } from "../../lib/agents";
import { clockShort, dayLabel } from "../../lib/format";
import { CopyGlyph, CloseGlyph } from "../../lib/icons";
import { DialogShell } from "../ui";
import { ChangeTree } from "./ChangeTree";
import { creditedAgents } from "./co-author";
import { buildPathTree } from "./path-tree";
import type { Commit, CommitFile, GitClient, StatusLetter } from "./types";

export interface CommitViewerProps {
  readonly commit: Commit;
  /** `null` while git is still being asked what it touched. */
  readonly files: readonly CommitFile[] | null;
  readonly client: GitClient | null;
  readonly sessionPaths: ReadonlySet<string>;
  readonly currentTimestamp: number;
  readonly onClose: () => void;
  readonly onReceipt: (text: string) => void;
}

type DiffState =
  | { readonly status: "reading" }
  | { readonly status: "ready"; readonly unified: string }
  | { readonly status: "failed"; readonly reason: string };

export function CommitViewer({
  commit,
  files,
  client,
  sessionPaths,
  currentTimestamp,
  onClose,
  onReceipt,
}: CommitViewerProps) {
  const rows = useMemo(
    () => buildPathTree((files ?? []).map((file) => file.path)),
    [files],
  );
  const letters = useMemo(() => {
    const out = new Map<string, StatusLetter>();
    for (const file of files ?? []) out.set(file.path, file.status);
    return out;
  }, [files]);

  // The first file, chosen for the reader: a commit viewer that opens on an
  // empty right pane makes them click twice to see what they came for.
  const [selected, setSelected] = useState<string | null>(null);
  const firstPath = files?.[0]?.path ?? null;
  useEffect(() => {
    setSelected((current) => (current === null ? firstPath : current));
  }, [firstPath]);

  const [diff, setDiff] = useState<DiffState>({ status: "reading" });
  useEffect(() => {
    if (selected === null || client === null) return;
    let cancelled = false;
    setDiff({ status: "reading" });
    void client
      .diff({ path: selected, sha: commit.sha })
      .then((answer) => {
        if (cancelled) return;
        if (answer.error !== null) setDiff({ status: "failed", reason: answer.error });
        else setDiff({ status: "ready", unified: answer.text });
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setDiff({
          status: "failed",
          reason: error instanceof Error ? error.message : "the diff failed",
        });
      });
    return () => {
      cancelled = true;
    };
  }, [selected, client, commit.sha]);

  const agents = creditedAgents(commit.coAuthors);
  const drawn = new Set(agents.map((agent) => agent.label));
  const people = [commit.author, ...commit.coAuthors.map((author) => author.name)].filter(
    (name) => name !== "" && !drawn.has(name),
  );
  const selectedName = selected === null ? null : selected.slice(selected.lastIndexOf("/") + 1);

  return (
    <DialogShell title={`${commit.short} — ${commit.subject}`} onClose={onClose} wide>
      <header className="ah-fileview__head ah-commitview__head">
        <div className="ah-fileview__title">
          <strong className="ah-commitview__subject" title={commit.subject}>
            {commit.subject}
          </strong>
        </div>
        <div className="ah-fileview__acts">
          <button
            type="button"
            className="ah-iconbtn"
            title="Copy the full sha"
            aria-label="Copy the full sha"
            onClick={() => {
              void navigator.clipboard
                .writeText(commit.sha)
                .then(() => onReceipt("sha copied"))
                .catch(() => onReceipt("the clipboard refused"));
            }}
          >
            <CopyGlyph />
          </button>
          <button
            type="button"
            className="ah-iconbtn"
            title="Close (Esc)"
            aria-label="Close"
            onClick={onClose}
          >
            <CloseGlyph />
          </button>
        </div>
        <p className="ah-commitview__meta">
          {people.map((name) => (
            <span key={name}>{name}</span>
          ))}
          {agents.map((agent) => (
            <AgentMark key={agent.kind} kind={agent.kind} note={agent.label} />
          ))}
          <code>{commit.short}</code>
          {!commit.merge && (
            <span className="tnum">
              +{commit.added} −{commit.deleted}
            </span>
          )}
          <time
            className="tnum"
            dateTime={new Date(commit.timestampMs).toISOString()}
          >
            {dayLabel(commit.timestampMs, currentTimestamp)} {clockShort(commit.timestampMs)}
          </time>
        </p>
      </header>

      <div className="ah-commitview">
        <div className="ah-commitview__files">
          <div className="ah-group">
            {files === null
              ? "Files"
              : `${files.length} changed ${files.length === 1 ? "file" : "files"}`}
          </div>
          <div className="ah-commitview__list">
            <ChangeTree
              rows={rows}
              letters={letters}
              sessionPaths={sessionPaths}
              selectedPath={selected}
              onSelect={(row) => row.kind === "file" && setSelected(row.path)}
              // A commit is history: nothing here stages, unstages or
              // discards, because those act on the working tree.
              action={null}
              onDiscard={null}
            />
          </div>
        </div>

        <div className="ah-commitview__diff">
          {selected === null ? (
            <div className="ah-fileview__state">
              <b>{files === null ? "Reading the commit" : "This commit touched no files"}</b>
              <span>
                {files === null
                  ? "Asking git what it changed."
                  : commit.merge
                    ? "It is a merge, and its parents carry the work."
                    : "An empty commit."}
              </span>
            </div>
          ) : (
            <>
              <p className="ah-commitview__path ah-idname" title={selected}>
                <bdi>{selected}</bdi>
              </p>
              {diff.status === "ready" ? (
                <DiffBody unified={diff.unified} name={selectedName ?? selected} />
              ) : diff.status === "failed" ? (
                <div className="ah-fileview__state">
                  <b>Could not read that change</b>
                  <span>{diff.reason}</span>
                </div>
              ) : (
                <div className="ah-fileview__state" data-waiting>
                  <b>Reading</b>
                  <span>Asking git for this file's change.</span>
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </DialogShell>
  );
}
