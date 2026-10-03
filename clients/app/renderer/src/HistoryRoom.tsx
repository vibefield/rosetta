// The History room, behind the ports a host hands it (`ports.ts`): it opens
// no socket and no fetch of its own. Columns change content, never geometry
// (H-2); the ledger's scale is global and stable whatever the filter says. Search is a
// tool held over the room, not a mode that replaces it (search record,
// 2026-09-09): the launcher in the strip opens a sheet, a hit lands in the
// transcript, and the four columns never change what they hold for it. The
// worktree column is a guest and the measure is not (H-3) — see
// `components/worktree/use-worktree-guest`, which owns that rule and the
// engine lifecycle behind it.
import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { CSSProperties, ForwardedRef, ReactNode, RefObject } from "react";
import type { SearchHit, SessionRow } from "@rosetta/protocol";
import type { RoomPorts } from "./ports";
import type { KeyValueStore } from "./lib/room-storage";
import { RoomProvider, useRoom, useRoomOverlayOpen } from "./room-context";
import type { SummaryTab } from "./room-context";
import { ColdStartPanel } from "./components/ColdStart";
import { ProjectsColumn } from "./components/projects";
import type { ProjectSort } from "./components/projects";
import { SearchLauncher, SearchSheet } from "./components/search";
import { SessionsColumn } from "./components/sessions";
import type { SessionFacts } from "./components/sessions";
import { TranscriptColumn } from "./components/Transcript";
import type { MatchStepperState, ThreadFocus } from "./components/Transcript";
import { EmptyState } from "./components/ui";
import { sessionFilePaths } from "./components/git";
import { useWorktreeGuest, WorktreeColumn } from "./components/worktree";
import {
  useDaemonStatus,
  useMeta,
  useProjectStats,
  useProjectUsage,
  useSearch,
  useTimeline,
} from "./lib/hooks";
import { useIngestActivity } from "./lib/use-ingest-activity";
import { isIngestActive } from "./lib/ingest";
import {
  UNASSIGNED_PROJECT_ID,
  buildProjectViewModel,
  getActivityGridStartTimestamp,
  projectKey,
  unassignedProject,
} from "./lib/model";
import type { ProjectViewModel } from "./lib/model";

import { compareOrderedSessions } from "./lib/activity-order";
import { useActivityOrder } from "./lib/use-activity-order";
import { createSearchResultsViewModel } from "./lib/search-results";
import {
  DEFAULT_SEARCH_FILTERS,
  hitsInReadingOrder,
  rememberQuery,
  sessionResultOf,
} from "./lib/search-sheet";
import type { SearchFilters } from "./lib/search-sheet";
import { spendOf } from "./lib/spend";

function useNow(refreshKey: unknown): number {
  const [tick, setTick] = useState(() => Date.now());
  useEffect(() => {
    const refresh = () => setTick(Date.now());
    const t = setInterval(refresh, 30_000);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      clearInterval(t);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, []);
  // Incoming metadata gets a fresh clock in the same render. Keep it stable
  // for unrelated UI changes so project aggregates do not recompute per keypress.
  return useMemo(() => Date.now(), [tick, refreshKey]);
}

/** The stepper's memory: which session's matches, in reading order, and
 * where the reader is among them. */
interface StepperMemory {
  readonly sessionId: string;
  readonly hits: readonly SearchHit[];
  readonly at: number;
}


/** The room's own search: the strip's launcher and the sheet it opens. A host
 * that offers search its own way (VibeField's Peek) leaves this out, and the
 * room then draws neither. */
export interface HistoryRoomSearch {
  /** Whether the sheet is open. The host owns it, so its own keys can open it. */
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
  /** The query the room opens with (rosetta's app reads `?q=`). */
  readonly initialQuery?: string | undefined;
  /** Drawn at the strip's end, after the launcher. */
  readonly accessory?: ReactNode;
}

export interface HistoryRoomProps extends RoomPorts {
  /** Who paints beneath the room. `paper` (the default): the room paints its
   * own ground and plate, as rosetta's app shows it. `host`: the host paints
   * its own surface beneath; the room paints neither, and its fades become
   * frost over whatever the host painted. */
  readonly surface?: "paper" | "host" | undefined;
  /** Where the room's dialogs and its sheet are portalled (the host's frame,
   * so its overlays anchor there); the document's body when absent. */
  readonly portalContainer?: HTMLElement | null | undefined;
  /** Every name the room keeps in storage goes under this prefix: recent
   * searches, dealt project tints, the site-icon setting, the fold. Default
   * `ah.`, which is where rosetta's app has always kept them. */
  readonly storagePrefix?: string | undefined;
  /** The store those names live in: the browser's localStorage when absent;
   * null keeps nothing across runs. */
  readonly storage?: KeyValueStore | null | undefined;
  readonly search?: HistoryRoomSearch | undefined;
  /** The chart the project panel opens on. */
  readonly initialSummaryTab?: SummaryTab | undefined;
  /** The host's words under "rosettad is not answering" when the catalog
   * itself has none — rosetta's app says where it looked and how to start one. */
  readonly unavailable?: ReactNode;
  /** What the room shows while the index waits for consent to read anything.
   * Absent: the room says it is waiting. */
  readonly consent?: ReactNode;
  /** The selected session, or null — on mount and on every change. */
  readonly onSelect?: ((selection: RoomSelection | null) => void) | undefined;
  /** Edges kept clear inside the room's frame for the host's chrome. */
  readonly edgeInset?: RoomInset | undefined;
  /** Scrollable space after every list's last row (`--ah-scroll-foot`), and
   * where floating controls rest (`--ah-floating-foot`): CSS lengths. */
  readonly scrollFoot?: string | undefined;
  readonly floatingFoot?: string | undefined;
}

/** The session the reader has selected, as a host acts on it (Resume). */
export interface RoomSelection {
  readonly sessionId: string;
  /** The agent's own id for the conversation: what a host resumes by. */
  readonly nativeSessionId: string;
  /** Where the session ran; null when the index does not know. */
  readonly folder: string | null;
}

/** A place in the record: a session, and optionally one of its messages. */
export interface RoomTarget {
  readonly sessionId: string;
  readonly timelineId?: string | null | undefined;
}

/** The room's edges kept clear for the host's own chrome, in pixels. */
export type RoomInset =
  | number
  | {
      readonly top?: number | undefined;
      readonly right?: number | undefined;
      readonly bottom?: number | undefined;
      readonly left?: number | undefined;
    };

/** What a host may ask of a mounted room. */
export interface HistoryRoomHandle {
  /** Walk the landed session's search matches. False when there is nothing to
   * walk, so the host's key can fall through. */
  stepMatch(delta: 1 | -1): boolean;
  /** Open a session — its thread, in its project — and land on one of its
   * messages when given. A session the index has not delivered yet is opened
   * when it arrives; one it does not have is said so in a receipt. */
  openAt(target: RoomTarget): void;
}

function insetStyle(inset: RoomInset | undefined): CSSProperties {
  if (inset === undefined) return {};
  const sides = typeof inset === "number" ? { top: inset, right: inset, bottom: inset, left: inset } : inset;
  return Object.fromEntries(
    (["top", "right", "bottom", "left"] as const)
      .filter((side) => sides[side] !== undefined)
      .map((side) => [`--ah-inset-${side}`, `${sides[side]}px`]),
  ) as CSSProperties;
}

/** The element's laid-out width, before the first paint and on every resize;
 * a transform (the stage receding) does not change it. */
function useLayoutWidth(ref: RefObject<HTMLElement | null>): number | null {
  const [width, setWidth] = useState<number | null>(null);
  useLayoutEffect(() => {
    const node = ref.current;
    if (node === null) return;
    setWidth(node.clientWidth);
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry !== undefined) setWidth(entry.contentRect.width);
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}

/** The room. A host hands it its ports; everything inside reads them from context. */
export const HistoryRoom = forwardRef<HistoryRoomHandle, HistoryRoomProps>(function HistoryRoom(props, ref) {
  const { catalog, sessions, assets, host } = props;
  const ports = useMemo<RoomPorts>(
    () => ({ catalog, sessions, assets, ...(host === undefined ? {} : { host }) }),
    [catalog, sessions, assets, host],
  );
  return (
    <RoomProvider
      ports={ports}
      options={{
        initialSummaryTab: props.initialSummaryTab,
        portalContainer: props.portalContainer,
        storagePrefix: props.storagePrefix,
        storage: props.storage,
      }}
    >
      <RoomBody
        handle={ref}
        surface={props.surface ?? "paper"}
        search={props.search ?? null}
        unavailable={props.unavailable ?? null}
        consent={props.consent ?? null}
        onSelect={props.onSelect}
        frameStyle={{
          ...insetStyle(props.edgeInset),
          ...(props.scrollFoot === undefined ? {} : { "--ah-scroll-foot": props.scrollFoot }),
          ...(props.floatingFoot === undefined ? {} : { "--ah-floating-foot": props.floatingFoot }),
        } as CSSProperties}
      />
    </RoomProvider>
  );
});

function RoomBody({
  handle,
  surface,
  search: searchOption,
  unavailable,
  consent,
  onSelect,
  frameStyle,
}: {
  readonly handle: ForwardedRef<HistoryRoomHandle>;
  readonly surface: "paper" | "host";
  readonly search: HistoryRoomSearch | null;
  readonly unavailable: ReactNode;
  readonly consent: ReactNode;
  readonly onSelect: ((selection: RoomSelection | null) => void) | undefined;
  readonly frameStyle: CSSProperties;
}) {
  const { catalog, sessions, host, storage, tints } = useRoom();
  const overlayOpen = useRoomOverlayOpen();
  const { meta, snapshotVersion, status: metaStatus } = useMeta(catalog);
  const nowMs = useNow(meta);
  const daemonStatus = useDaemonStatus(catalog);
  const {
    connectionError,
    canonicalReady,
    searchReadiness: readiness,
    searchProgress,
    ingest,
  } = daemonStatus;
  const activity = useIngestActivity(daemonStatus);
  const ingestActive = isIngestActive(ingest);
  const activityOrder = useActivityOrder(
    meta, snapshotVersion, metaStatus === "live" && canonicalReady === true && !ingestActive,
    canonicalReady !== true || ingestActive, nowMs,
  );
  const [query, setQuery] = useState(() => searchOption?.initialQuery ?? "");
  const sheetOpen = searchOption?.open ?? false;
  const setSheetOpen = (open: boolean) => searchOption?.onOpenChange(open);
  const [filters, setFilters] = useState<SearchFilters>(DEFAULT_SEARCH_FILTERS);
  const [stepper, setStepper] = useState<StepperMemory | null>(null);
  const [sort, setSort] = useState<ProjectSort>("recent");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [inspectedThread, setInspectedThread] = useState<{ rootId: string; id: string } | null>(null);
  const [focus, setFocus] = useState<ThreadFocus | null>(null);
  const [receipt, setReceipt] = useState<string | null>(null);

  // "This project" is only a scope while there is one; the unassigned
  // pseudo-project names no project the index could select by.
  const scopedProjectId =
    filters.scope === "project" && projectId !== null && projectId !== UNASSIGNED_PROJECT_ID
      ? projectId
      : null;
  const search = useSearch(catalog, query, {
    project: scopedProjectId,
    kind: filters.kind === "any" ? null : filters.kind,
    agent: filters.agent,
  });

  const projectIds = useMemo(() => {
    const ids = (meta?.projects ?? []).map((p) => p.id);
    if ((meta?.sessions ?? []).some((session) => session.project_id === null)) {
      ids.push(UNASSIGNED_PROJECT_ID);
    }
    return ids.sort();
  }, [meta]);
  // Cross-session figures wait for canonical publication. Metadata and
  // completed timelines remain progressive; aggregate scans never compete
  // with the sole cold writer for the shared RAM projection.
  const stats = useProjectStats(catalog, projectIds, canonicalReady === true);

  const projectRows = useMemo(() => {
    const rows = [...(meta?.projects ?? [])];
    if ((meta?.sessions ?? []).some((session) => session.project_id === null)) {
      rows.push(unassignedProject());
    }
    return rows;
  }, [meta]);

  const kindOf = useMemo(() => {
    const byId = new Map((meta?.runtimes ?? []).map((r) => [r.id, r.kind]));
    return (runtimeId: string) => byId.get(runtimeId) ?? runtimeId;
  }, [meta]);

  const home = useMemo(() => {
    // The desktop shell knows the home directory outright. The daemon only
    // knows agent data roots, and it now reports them already shortened
    // (`~/.claude`) — a string that names no home — so derive from one only
    // when it is still absolute, and otherwise show paths in full.
    const fromShell = host?.homeDir ?? null;
    if (fromShell !== null) return fromShell;
    const root = meta?.runtimes[0]?.data_root ?? null;
    if (root === null || !root.startsWith("/")) return null;
    return root.replace(/\/\.[^/]+$/, "");
  }, [meta, host]);

  const projectViewModels: ProjectViewModel[] = useMemo(
    () =>
      projectRows.map((p) =>
        buildProjectViewModel(p, meta?.sessions ?? [], stats.get(p.id), kindOf, nowMs, tints.projectTint),
      ),
    [projectRows, meta, stats, kindOf, nowMs, tints],
  );
  const projectById = useMemo(
    () => new Map(projectViewModels.map((project) => [project.row.id, project])),
    [projectViewModels],
  );

  // Per-session facts (tokens + messages) from every loaded contribution.
  const facts = useMemo(() => {
    const out = new Map<string, SessionFacts>();
    for (const s of stats.values()) {
      for (const row of s.contribution) {
        out.set(row.session_id, {
          tokens: row.tokens,
          messages: row.messages,
          spend: spendOf(row),
        });
      }
    }
    return out;
  }, [stats]);

  // Selection is always an explicit reader act. Metadata may invalidate an
  // existing selection, but arriving data never chooses a project for them.
  useEffect(() => {
    if (projectId !== null && !projectById.has(projectId)) {
      setProjectId(null);
      setSessionId(null);
    }
  }, [projectById, projectId]);

  const projectSessions = useMemo(() => {
    if (projectId === null) return [];
    return (meta?.sessions ?? [])
      .filter(
        (s) =>
          projectKey(s) === projectId &&
          s.parent_session_id === null,
      )
      .sort((left, right) => compareOrderedSessions(left, right, activityOrder));
  }, [meta, projectId, activityOrder]);

  useEffect(() => {
    if (sessionId !== null && !projectSessions.some((s) => s.id === sessionId)) {
      setSessionId(null);
    }
  }, [projectSessions, sessionId]);

  const viewedSessionId = inspectedThread?.rootId === sessionId ? inspectedThread.id : sessionId;
  const session = useMemo(
    () => (meta?.sessions ?? []).find((s) => s.id === viewedSessionId) ?? null,
    [meta, viewedSessionId],
  );
  const openThread = useCallback((id: string) => {
    if (sessionId !== null) setInspectedThread({ rootId: sessionId, id });
  }, [sessionId]);
  // Attention-directed ingestion: opening a not-yet-indexed session moves
  // its queued work to the front of the backfill queue. Under slicing the
  // common case is "streaming" — the starter landed and the completion
  // remainder is still queued — and that remainder is exactly what the
  // click promotes; "pending" covers a session no pass has reached at all.
  // Advisory — a session with nothing queued is a no-op, an old daemon just
  // keeps its order, and the ledger dedupes repeats.
  const sessionState = session?.stream_state ?? null;
  useEffect(() => {
    if (viewedSessionId === null || sessionState === null || sessionState === "indexed") return;
    if (ingest?.phase !== "backfilling") return;
    catalog.boost?.(viewedSessionId).catch(() => {});
  }, [catalog, viewedSessionId, sessionState, ingest?.phase]);
  const feed = useTimeline(sessions, viewedSessionId);
  const usage = useProjectUsage(catalog, projectId, canonicalReady === true);

  const projectRoot = useMemo(() => {
    if (projectId === null) return null;
    const roots = projectById.get(projectId)?.row.root_paths ?? [];
    if (session?.cwd) {
      const matching = roots
        .filter((root) => session.cwd === root || session.cwd?.startsWith(`${root}/`))
        .sort((a, b) => b.length - a.length)[0];
      if (matching) return matching;
    }
    return roots[0] ?? null;
  }, [projectId, projectById, session]);

  // Which files this session touched — the one fact a git client cannot show.
  // Empty until the typed tool projection ships `file_change` actions, which
  // draws no marks rather than guessing at them.
  const sessionPaths = useMemo(
    () => sessionFilePaths(feed.rows, projectRoot),
    [feed.rows, projectRoot],
  );

  /* ── column ③, a guest per H-3 ──────────────────────────────────────────
     Files, changes and history are one column now, so the room is back to
     one guest and the record's original rule applies unchanged: below a
     1360px frame it closes rather than squeeze the transcript. */
  const roomRef = useRef<HTMLDivElement | null>(null);
  const roomWidth = useLayoutWidth(roomRef);
  const worktree = useWorktreeGuest(projectRoot, roomWidth);

  // The host hears which session is selected — what its own acts (Resume)
  // would continue — on mount and whenever it changes.
  const selected = useMemo(
    () => (sessionId === null ? null : ((meta?.sessions ?? []).find((s) => s.id === sessionId) ?? null)),
    [meta, sessionId],
  );
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const selectionKey = selected === null ? null : `${selected.id}\u0000${selected.native_session_id}\u0000${selected.cwd ?? ""}`;
  useEffect(() => {
    onSelectRef.current?.(
      selected === null
        ? null
        : { sessionId: selected.id, nativeSessionId: selected.native_session_id, folder: selected.cwd },
    );
    // `selectionKey` is the selection's identity; `selected` is read with it.
  }, [selectionKey]);

  /* ── search: one view model, held over the room ─────────────────────── */
  const searchResults = useMemo(() => {
    if (search.query === "" || search.response === null || meta === null) return null;
    return createSearchResultsViewModel(search.response.hits, meta.sessions, projectById);
  }, [search.query, search.response, meta, projectById]);

  // Opening a hit is a continuation, not a navigation: the sheet leaves, the
  // room opens the thread, the transcript lands on the message, and the
  // stepper remembers the session's other matches.
  const openHit = (rootSession: SessionRow, hit: SearchHit) => {
    rememberQuery(storage, query);
    setSheetOpen(false);
    setProjectId(projectKey(rootSession));
    setSessionId(rootSession.id);
    setInspectedThread(null);
    setFocus({ sessionId: hit.session_id, timelineId: hit.timeline_id });
    const result = searchResults === null ? null : sessionResultOf(searchResults, rootSession.id);
    const hits = result === null ? [hit] : hitsInReadingOrder(result.hits);
    const at = hits.findIndex(
      (h) => h.timeline_id === hit.timeline_id && h.session_id === hit.session_id,
    );
    setStepper({ sessionId: rootSession.id, hits, at: Math.max(0, at) });
  };

  const stepMatch = useCallback(
    (delta: 1 | -1) => {
      setStepper((current) => {
        if (current === null || current.hits.length === 0) return current;
        const at = (current.at + delta + current.hits.length) % current.hits.length;
        const hit = current.hits[at]!;
        setInspectedThread(null);
        setFocus({ sessionId: hit.session_id, timelineId: hit.timeline_id });
        return { ...current, at };
      });
    },
    [],
  );
  // A host's "open this": the thread the session belongs to, in its project,
  // landed on the message when one is named. It waits for the index to
  // deliver the session, and says so when the index does not have it.
  const [pendingOpen, setPendingOpen] = useState<RoomTarget | null>(null);
  useEffect(() => {
    if (pendingOpen === null || meta === null) return;
    const target = meta.sessions.find((s) => s.id === pendingOpen.sessionId) ?? null;
    if (target === null) {
      if (metaStatus !== "live") return;
      setPendingOpen(null);
      showReceipt("That session is not in this index");
      return;
    }
    const root = target.parent_session_id === null
      ? target
      : (meta.sessions.find((s) => s.id === target.root_session_id) ?? target);
    setPendingOpen(null);
    setProjectId(projectKey(root));
    setSessionId(root.id);
    setInspectedThread(null);
    setStepper(null);
    const timelineId = pendingOpen.timelineId ?? null;
    setFocus(timelineId === null ? null : { sessionId: target.id, timelineId });
  }, [pendingOpen, meta, metaStatus]);

  // ⌘G / ⇧⌘G belong to the host's window; it asks the room to walk.
  const stepping = useRef(false);
  stepping.current = stepper !== null;
  useImperativeHandle(handle, () => ({
    stepMatch(delta) {
      if (!stepping.current) return false;
      stepMatch(delta);
      return true;
    },
    openAt(target) {
      setPendingOpen({ sessionId: target.sessionId, timelineId: target.timelineId ?? null });
    },
  }), [stepMatch]);

  // The stepper belongs to the session it was born in.
  useEffect(() => {
    if (stepper !== null && stepper.sessionId !== sessionId) setStepper(null);
  }, [sessionId, stepper]);

  const clearSearch = () => {
    setQuery("");
    setStepper(null);
  };


  const showReceipt = (text: string) => {
    setReceipt(text);
    window.setTimeout(() => setReceipt(null), 2600);
  };

  const kinds = useMemo(() => (meta?.runtimes ?? []).map((r) => r.kind), [meta]);
  const threadCount = useMemo(
    () => (meta?.sessions ?? []).filter((s) => s.parent_session_id === null).length,
    [meta],
  );
  // The launcher states the scope in a breath; the sheet's foot states it in
  // full — the strip is always honest and never furniture (Q5, answered).
  const launcherPlaceholder =
    meta === null || threadCount === 0
      ? "Search your whole agentic past"
      : `Search ${threadCount.toLocaleString("en-US")} sessions across ${kinds.length} ${kinds.length === 1 ? "agent" : "agents"}`;
  const stepperState: MatchStepperState | null =
    stepper === null || stepper.sessionId !== sessionId
      ? null
      : { query: search.query, at: stepper.at, total: stepper.hits.length };

  const disconnected = meta === null;
  // The roll call: discovery is on the board and nothing has been read.
  const awaitingConsent = ingest?.phase === "awaiting_consent";
  // The first run before its first metadata batch: the daemon answers and
  // meters move, but there is not yet a canonical project to render. Once
  // RAM metadata arrives the ordinary room takes over progressively.
  const coldStarting =
    projectRows.length === 0 && ingestActive && ingest !== null && !awaitingConsent;

  return (
    <div
      className="ah-frame is-tall"
      data-ah-surface={surface}
      data-ah-overlay={overlayOpen ? "" : undefined}
      style={frameStyle}
    >
      <div className="ah-room" ref={roomRef}>
        {searchOption !== null && (
          <div className="ah-searchbar">
            <SearchLauncher
              query={search.query}
              placeholder={launcherPlaceholder}
              onOpen={() => setSheetOpen(true)}
              onClear={clearSearch}
            />
            {searchOption.accessory}
          </div>
        )}
        {disconnected && !coldStarting ? (
          <EmptyState
            style={{ flex: 1 }}
            title={
              metaStatus === "closed"
                ? "rosettad is not answering"
                : "Connecting to rosettad"
            }
            description={connectionError ?? unavailable}
          >
            <div className="ah-meter">
              <i style={{ width: metaStatus === "connecting" ? "30%" : "8%" }} />
            </div>
          </EmptyState>
        ) : awaitingConsent ? (
          consent ?? (
            <EmptyState
              style={{ flex: 1 }}
              title="Waiting for consent"
              description="The index has found agents on this machine and reads nothing until it is told which ones to read."
            />
          )
        ) : coldStarting ? (
          <ColdStartPanel activity={activity} />
        ) : (
          <div className="ah-cols" data-rail={worktree.visible ? "on" : "off"}>
            <ProjectsColumn
              projects={projectViewModels}
              activityOrder={activityOrder}
              selectedProjectId={projectId}
              sort={sort}
              onSortChange={setSort}
              homeDirectory={home}
              currentTimestamp={nowMs}
              ingest={ingest}
              activity={activity}
              onProjectSelect={(id) => {
                setProjectId(id);
                setSessionId(null);
                setInspectedThread(null);
              }}
            />
            <SessionsColumn
              project={projectId ? (projectById.get(projectId) ?? null) : null}
              sessions={projectSessions}
              factsBySessionId={facts}
              getRuntimeKind={kindOf}
              selectedSessionId={sessionId}
              usage={usage}
              activityGridStartTimestamp={getActivityGridStartTimestamp(nowMs)}
              currentTimestamp={nowMs}
              ingest={ingest}
              isWorktreeVisible={
                worktree.available && projectRoot !== null ? worktree.visible : null
              }
              onToggleWorktree={worktree.toggle}
              onSessionSelect={(selectedSession) => { setSessionId(selectedSession.id); setInspectedThread(null); }}
            />
            {/* Always in the grid — a closed guest is a 0px track, and the
                measure takes back whatever it gives up. */}
            <WorktreeColumn
              projectRoot={projectRoot}
              tint={projectId ? (projectById.get(projectId)?.tint ?? null) : null}
              homeDirectory={home}
              sessionPaths={sessionPaths}
              currentTimestamp={nowMs}
              onReceipt={showReceipt}
              requestError={worktree.error}
              onRetry={worktree.retry}
            />
            <TranscriptColumn
              session={session}
              duringBackfill={ingestActive}
              sessions={meta?.sessions ?? []}
              project={session ? (projectById.get(projectKey(session)) ?? null) : null}
              feed={feed}
              focus={focus}
              onFocusConsumed={() => setFocus(null)}
              onReceipt={showReceipt}
              stepper={stepperState}
              onStepMatch={stepMatch}
              onDismissStepper={() => setStepper(null)}
              getRuntimeKind={kindOf}
              homeDirectory={home}
              onOpenThread={openThread}
              onBackToParent={session?.parent_session_id == null ? null : () => openThread(session.parent_session_id!)}
            />
          </div>
        )}
      </div>
      {searchOption !== null && <SearchSheet
        open={sheetOpen}
        onClose={() => setSheetOpen(false)}
        query={query}
        onQuery={setQuery}
        filters={filters}
        onFilters={setFilters}
        feed={search}
        results={searchResults}
        readiness={readiness}
        progress={searchProgress}
        canonicalReady={canonicalReady}
        ingestPaused={ingest?.phase === "metadata_ready"}
        agentKinds={kinds}
        currentProject={
          projectId !== null && projectId !== UNASSIGNED_PROJECT_ID
            ? (projectById.get(projectId) ?? null)
            : null
        }
        homeDirectory={home}
        nowMs={nowMs}
        getRuntimeKind={kindOf}
        onOpenHit={openHit}
      />}
      {receipt !== null && <div className="ah-receipt">{receipt}</div>}
    </div>
  );
}
