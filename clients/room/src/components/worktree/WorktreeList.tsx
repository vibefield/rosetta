// The repository's other checkouts, under the tree that is showing one of
// them. Only worth drawing when there is more than one: a repository with a
// single worktree has nothing to disambiguate, and a list of one is a label.
//
// It is a ledger of disk. A repository's worktrees hold the same tracked
// files, so what tells them apart on a disk is what git does not track (the
// installed packages, the build output), and a reader deciding which checkout
// to let go of wants that figure first. So the rows stand heaviest first,
// each a name with its size at the edge over a bar scaled to the heaviest;
// where the folder is and how dirty the open one is are on the row's hover.
//
// A FIGURE IS A WALK OF A FOLDER, AND A REPOSITORY CAN HAVE HUNDREDS OF
// WORKTREES. So the list asks its host only for the rows that are on screen,
// again as the reader scrolls, and a host keeps what it measured: the next
// opening shows those figures at once and walks nothing. A row whose figure
// is owed shows that it is being weighed, never a blank.
//
// Figures arrive one at a time while the list is being read. A row keeps its
// place until the list is at rest (at its top, no pointer or keyboard in it);
// only then does it take its rank (`settleOrder`).
//
// A row is named by its branch, or, with no branch, by its folder's own name
// with its commit beside it. The whole path of a branchless checkout was four
// lines of this column, with the one part that told two apart at the very end.
//
// A press copies the name. That is the room's one act here: switching
// worktrees is an act on a repository, and this room reads history.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { GitFacts, GitWorktree, WorktreeSize } from "../../ports";
import { fmtBytes, tildePath } from "../../lib/format";
import { CopyGlyph } from "../../lib/icons";
import { useCopyText } from "../../room-context";
import { ledgerOf, settleOrder } from "./worktree-ledger";

export interface WorktreeListProps {
  readonly facts: GitFacts;
  /** The root the tree is showing, which earns the dirty count. */
  readonly openRoot: string | null;
  readonly homeDirectory: string | null;
  /** The room's receipt line, so a copy is never silent. */
  readonly onReceipt: (text: string) => void;
  /** Ask the host what these worktrees hold on disk. Its identity is the
   * open root's: a new one starts the list's answers over. Absent: the host
   * measures nothing, and the list draws only the figures git facts carried. */
  readonly measure?: ((paths: readonly string[]) => Promise<readonly WorktreeSize[]>) | undefined;
}

/** How long a scroll settles before the rows on screen are asked for. */
const SETTLE_MS = 160;
/** How often the rows on screen that are still being weighed are asked again. */
const POLL_MS = 1500;
/** Rows one ask names at most: more than a column shows at once. */
const ASK_ROWS = 32;
/** Rows just off screen that count as shown, so a short scroll finds them weighed. */
const NEAR = "96px 0px";

/** What the host last said of a path: bytes, null while it walks, "none" for no figure. */
type Answer = number | null | "none";

export function WorktreeList({
  facts,
  openRoot,
  homeDirectory,
  onReceipt,
  measure,
}: WorktreeListProps) {
  const copy = useCopyText();
  const [answers, setAnswers] = useState<ReadonlyMap<string, Answer>>(new Map());
  useEffect(() => setAnswers(new Map()), [measure]);

  const ledger = useMemo(() => {
    const sizeOf = (worktree: GitWorktree): number | null | undefined => {
      const said = answers.get(worktree.path);
      // a figure the host already held stands while the folder is walked again
      const kept = typeof worktree.sizeBytes === "number" ? worktree.sizeBytes : undefined;
      if (said !== undefined) return said === "none" ? undefined : (said ?? kept ?? null);
      if (kept !== undefined) return kept;
      // owed: asked for once the row is on screen
      return measure === undefined ? undefined : null;
    };
    return ledgerOf(facts.worktrees, openRoot, sizeOf);
  }, [facts.worktrees, openRoot, answers, measure]);
  const byPath = useMemo(() => new Map(ledger.rows.map((row) => [row.path, row])), [ledger]);
  const ranked = useMemo(() => ledger.rows.map((row) => row.path), [ledger]);

  // ── the drawn order: the ledger's while at rest, each row's own place otherwise
  const scroller = useRef<HTMLDivElement>(null);
  const held = useRef({ pointer: false, keyboard: false, scrolled: false });
  const [restTick, setRestTick] = useState(0);
  const atRest = (): boolean =>
    !held.current.pointer && !held.current.keyboard && !held.current.scrolled;
  const hold = (what: "pointer" | "keyboard" | "scrolled", value: boolean): void => {
    if (held.current[what] === value) return;
    held.current[what] = value;
    if (atRest()) setRestTick((tick) => tick + 1);
  };
  const [order, setOrder] = useState<readonly string[]>([]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: restTick is the rest's own signal
  useEffect(() => {
    setOrder((drawn) => settleOrder(drawn, ranked, atRest()));
  }, [ranked, restTick]);
  const drawn = useMemo(() => settleOrder(order, ranked, false), [order, ranked]);

  // ── the rows on screen, and what the host is asked of them
  const shown = useRef(new Set<string>());
  const [seenTick, setSeenTick] = useState(0);
  useEffect(() => {
    const root = scroller.current;
    if (root === null || measure === undefined) return;
    const rows = [...root.querySelectorAll<HTMLElement>("[data-worktree]")];
    if (typeof IntersectionObserver === "undefined") {
      shown.current = new Set(rows.slice(0, 12).map((row) => row.dataset["worktree"] ?? ""));
      setSeenTick((tick) => tick + 1);
      return;
    }
    let timer: number | undefined;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const path = (entry.target as HTMLElement).dataset["worktree"];
          if (path === undefined) continue;
          if (entry.isIntersecting) shown.current.add(path);
          else shown.current.delete(path);
        }
        window.clearTimeout(timer);
        timer = window.setTimeout(() => setSeenTick((tick) => tick + 1), SETTLE_MS);
      },
      { root, rootMargin: NEAR },
    );
    shown.current = new Set();
    for (const row of rows) observer.observe(row);
    return () => {
      window.clearTimeout(timer);
      observer.disconnect();
    };
  }, [drawn, measure]);

  const settled = useRef(new Set<string>());
  const asking = useRef(false);
  const [pollTick, setPollTick] = useState(0);
  const gone = useMemo(
    () => new Set(ledger.rows.filter((row) => row.gone).map((row) => row.path)),
    [ledger],
  );
  // biome-ignore lint/correctness/useExhaustiveDependencies: the ticks are the signals
  useEffect(() => {
    if (measure === undefined || asking.current) return;
    const want = drawn
      .filter((path) => shown.current.has(path) && !gone.has(path) && !settled.current.has(path))
      .slice(0, ASK_ROWS);
    if (want.length === 0) return;
    let live = true;
    let again: number | undefined;
    asking.current = true;
    measure(want)
      .then((said) => {
        if (!live) return;
        let owed = false;
        const next = new Map<string, Answer>();
        for (const path of want) {
          const row = said.find((one) => one.path === path);
          const size = row?.sizeBytes;
          if (typeof size === "number") next.set(path, size);
          else if (size === null) next.set(path, null);
          else next.set(path, "none");
          if (size === null) owed = true;
          else settled.current.add(path);
        }
        setAnswers((before) => {
          const merged = new Map(before);
          for (const [path, answer] of next) merged.set(path, answer);
          return merged;
        });
        if (owed) again = window.setTimeout(() => setPollTick((tick) => tick + 1), POLL_MS);
      })
      .catch(() => {
        // the host could not say: the rows stay as they are, and the next
        // scroll asks again
      })
      .finally(() => {
        asking.current = false;
      });
    return () => {
      live = false;
      window.clearTimeout(again);
    };
  }, [seenTick, pollTick, measure, gone]);
  useEffect(() => {
    settled.current = new Set();
  }, [measure]);

  const press = useCallback(
    (name: string) => {
      void copy(name)
        .then(() => onReceipt("worktree name copied"))
        .catch(() => onReceipt("the clipboard refused"));
    },
    [copy, onReceipt],
  );

  // `ah-rail*` is the design record's name for this column; room.css is a
  // port of the proto and keeps its vocabulary.
  return (
    <div
      className="ah-railfoot"
      onPointerEnter={() => hold("pointer", true)}
      onPointerLeave={() => hold("pointer", false)}
      onFocusCapture={(event) => {
        if ((event.target as HTMLElement).matches(":focus-visible")) hold("keyboard", true);
      }}
      onBlurCapture={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) hold("keyboard", false);
      }}
    >
      <div className="ah-railsplit">
        <span className="ah-eyebrow">Worktrees</span>
        <span className="ah-railsplit__sum tnum">
          {ledger.rows.length}
          {ledger.totalBytes !== null &&
            ` · ${fmtBytes(ledger.totalBytes)}${ledger.partial ? "+" : ""}`}
        </span>
      </div>
      <div
        className="ah-railfoot__list"
        role="list"
        aria-label="Worktrees"
        ref={scroller}
        onScroll={(event) => hold("scrolled", event.currentTarget.scrollTop > 0)}
      >
        {drawn.map((path) => {
          const row = byPath.get(path);
          if (row === undefined) return null;
          // The count belongs to the checkout the shell actually read, and
          // git reports it for one worktree at a time.
          const dirty = row.open ? facts.dirty : null;
          const weighing = row.sizeBytes === null;
          const hover = [
            tildePath(row.path, homeDirectory),
            row.gone ? "its folder is gone" : null,
            dirty === null ? null : dirty > 0 ? `${dirty} dirty` : "clean",
            "Press to copy the name",
          ]
            .filter((part) => part !== null)
            .join(" · ");
          return (
            <div role="listitem" key={row.path}>
              <button
                type="button"
                className="ah-worktree"
                title={hover}
                data-worktree={row.path}
                data-open={row.open ? "" : undefined}
                data-gone={row.gone ? "" : undefined}
                data-weighing={weighing ? "" : undefined}
                aria-busy={weighing || undefined}
                onClick={() => press(row.name)}
              >
                <span className="ah-worktree__name">
                  <span className="ah-worktree__label">{row.name}</span>
                  {row.commit !== null && <code>{row.commit}</code>}
                  <span className="ah-worktree__copy">
                    <CopyGlyph />
                  </span>
                </span>
                <span className="ah-worktree__size tnum">
                  {row.gone ? (
                    "gone"
                  ) : typeof row.sizeBytes === "number" ? (
                    fmtBytes(row.sizeBytes)
                  ) : weighing ? (
                    <span className="ah-worktree__wait" role="img" aria-label="measuring" />
                  ) : null}
                </span>
                {(row.share !== null || weighing) && (
                  <span className="ah-worktree__bar" aria-hidden="true">
                    {row.share !== null && (
                      <span style={{ width: `${Math.max(row.share * 100, 1.5).toFixed(2)}%` }} />
                    )}
                  </span>
                )}
              </button>
            </div>
          );
        })}
      </div>
    </div>
  );
}
