// Column ③ — the project's worktree, mille's shipped tree mounted for real
// (design record §8). The column is named by the folder it is showing: a
// path is an id, so it sets in mono. The head carries no badge — we know how
// many files the tree is SHOWING, not how many the repository has. The rail
// is read-only in every direction: an empty command registry, no context
// menu, no drag — this room reads history, it does not edit worktrees.
import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, RefObject } from "react";
import type { FileExplorer } from "@vibecook/mille";
import { connectFileExplorer } from "@vibecook/mille/port";
import { FileTree, FileTreeProvider } from "@vibecook/mille-ui";
import { createCommandRegistry } from "@vibecook/mille-ui/commands";
import { vibefieldIconTheme } from "@vibecook/mille-ui/icons/vibefield";
import { tildePath } from "../lib/format";
import { host, onRailPort, takeRailPort } from "../lib/rail-port";
import type { GitFacts } from "../lib/rail-port";
import { EmptyState } from "./ui";

interface Connection {
  fx: FileExplorer;
  root: string;
  dispose: () => void;
}

/** One engine connection at a time; every arrival of a fresh port (a project
 * switch re-forked the utility) swaps it. Lives outside per-render state so
 * StrictMode's double effects reuse rather than re-claim the port. */
function useRailEngine(): Connection | null {
  const [conn, setConn] = useState<Connection | null>(null);
  useEffect(() => {
    let current: Connection | null = null;
    let cancelled = false;
    const stop = onRailPort((msg) => {
      takeRailPort(msg);
      void (async () => {
        try {
          const client = await connectFileExplorer(msg.port, { mirrorCap: 20_000 });
          if (cancelled) {
            client.dispose?.();
            return;
          }
          current?.dispose();
          current = {
            fx: client as unknown as FileExplorer,
            root: msg.root,
            dispose: () => (client as { dispose?: () => void }).dispose?.(),
          };
          setConn(current);
        } catch {
          // a dead port (superseded switch) — the next arrival wins
        }
      })();
    });
    return () => {
      cancelled = true;
      stop();
      current?.dispose();
    };
  }, []);
  return conn;
}

function useGitFacts(root: string | null): GitFacts | null {
  const [facts, setFacts] = useState<GitFacts | null>(null);
  useEffect(() => {
    setFacts(null);
    const bridge = host();
    if (root === null || bridge === null) return;
    let cancelled = false;
    const load = () => {
      bridge.gitFacts(root).then((f) => {
        if (!cancelled) setFacts(f);
      });
    };
    load();
    const timer = setInterval(load, 15_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [root]);
  return facts;
}

/** The tree's own height, so the worktrees below it sit against the last row
 * rather than at the bottom of the column (design record §8 draws the rail as
 * one flow: tree, hairline, worktrees). A virtualized tree cannot size itself
 * — it needs a definite height to have a viewport at all — so measure what it
 * is drawing and hand that back. `null` means "not measurable", and the CSS
 * then fills the column, which is also what a tree taller than the rail
 * wants; the wrapper's `flex-shrink` caps the measured height there. */
function useTreeContentHeight(ready: boolean): {
  ref: RefObject<HTMLDivElement | null>;
  height: number | null;
} {
  const ref = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState<number | null>(null);
  useEffect(() => {
    setHeight(null);
    const wrapper = ref.current;
    if (!ready || wrapper === null || typeof MutationObserver === "undefined") return;

    let frame = 0;
    // The scroller's only child is the virtualizer's sizer, and its height is
    // the whole tree's — the rows inside it are a viewport-sized subset, so
    // counting them would measure the window, not the document.
    const measure = (): void => {
      frame = 0;
      const scroller = wrapper.querySelector<HTMLElement>('[role="tree"]');
      const sizer = scroller?.firstElementChild;
      if (!(scroller instanceof HTMLElement) || !(sizer instanceof HTMLElement)) {
        setHeight(null);
        return;
      }
      const style = getComputedStyle(scroller);
      const padding =
        (parseFloat(style.paddingTop) || 0) + (parseFloat(style.paddingBottom) || 0);
      const next = sizer.offsetHeight + padding;
      setHeight((current) => (current === next ? current : next));
    };
    const schedule = (): void => {
      if (frame === 0) frame = requestAnimationFrame(measure);
    };

    measure();
    // Rows come and go with the viewport, so this fires on every scroll frame
    // as well as on every expand — the rAF collapses a burst into one read,
    // and an unchanged height sets no state.
    const observer = new MutationObserver(schedule);
    observer.observe(wrapper, { childList: true, subtree: true, attributeFilter: ["style"] });
    return () => {
      observer.disconnect();
      if (frame !== 0) cancelAnimationFrame(frame);
    };
  }, [ready]);
  return { ref, height };
}

export function RailColumn({
  projectRoot,
  tint,
  home,
}: {
  /** The selected project's first root path, from rosettad. */
  projectRoot: string | null;
  tint: string | null;
  home: string | null;
}) {
  const conn = useRailEngine();
  // The engine follows the selection with a lag (kill, re-fork, walk); the
  // head names what the tree is actually showing, never what was asked for.
  const shownRoot = conn?.root ?? null;
  const facts = useGitFacts(shownRoot);
  const commands = useMemo(() => createCommandRegistry([]), []);
  const tree = useTreeContentHeight(conn !== null);
  const bridge = host();
  const stale = projectRoot !== null && shownRoot !== null && shownRoot !== projectRoot;

  const factText =
    facts === null || facts.branch === null
      ? null
      : `${facts.branch}${facts.dirty !== null ? ` · ${facts.dirty} dirty` : ""}`;

  return (
    <div className="ah-col ah-col--rail">
      <header>
        <div className="ah-colhead">
          <i style={tint ? ({ "--tint": tint } as CSSProperties) : undefined} />
          <strong className="ah-idname" title={shownRoot ?? undefined}>
            {shownRoot ? tildePath(shownRoot, home) : "—"}
          </strong>
        </div>
        {factText !== null && <span className="ah-caption tnum">{factText}</span>}
      </header>
      {bridge === null ? (
        <EmptyState
          title="No worktree here"
          description="The tree needs the desktop shell — a browser tab has no engine to walk it."
        />
      ) : conn === null ? (
        <EmptyState
          title="Opening the worktree"
          description="The engine is walking the project root."
        />
      ) : (
        <>
          <div
            ref={tree.ref}
            className="ah-railtree"
            // No `data-mille-density="compact"`: the design record's rail is
            // the shipped 26px row (§8, "26px rows rounded inside an 8px
            // gutter"). 22px is the density for a long editor rail.
            data-mille-theme="vibefield"
            data-stale={stale ? "" : undefined}
            style={tree.height !== null ? { height: tree.height } : undefined}
          >
            <FileTreeProvider fx={conn.fx} commands={commands}>
              <FileTree
                ariaLabel="Project worktree"
                iconTheme={vibefieldIconTheme}
                disableContextMenu
                disableDragDrop
                style={{ height: "100%" }}
              />
            </FileTreeProvider>
          </div>
          {facts !== null && facts.worktrees.length > 1 && (
            <div className="ah-railfoot">
              <div className="ah-railsplit">
                <span className="ah-eyebrow">Worktrees</span>
              </div>
              <div className="ah-railfoot__list">
                {facts.worktrees.map((wt) => (
                  <div className="ah-worktree" key={wt.path} title={wt.path}>
                    <span>{wt.branch ?? tildePath(wt.path, home)}</span>
                    {wt.path === shownRoot && facts.dirty !== null && (
                      <code data-dirty={facts.dirty > 0 ? "" : undefined} className="tnum">
                        {facts.dirty > 0 ? `${facts.dirty} dirty` : "clean"}
                      </code>
                    )}
                  </div>
                ))}
              </div>
            </div>
          )}
        </>
      )}
    </div>
  );
}
