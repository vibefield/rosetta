// The History room. Columns change content, never geometry (H-2); the
// ledger's scale is global and stable whatever the filter says. Search is a
// tool held over the room, not a mode that replaces it (search record,
// 2026-09-09): the launcher in the strip opens a sheet, a hit lands in the
// transcript, and the four columns never change what they hold for it. The
// worktree column is a guest and the measure is not (H-3) — see
// `components/worktree/use-worktree-guest`, which owns that rule and the
// engine lifecycle behind it.
import { useCallback, useEffect, useMemo, useState } from "react";
import type { SearchHit, SessionRow } from "@rosetta/protocol";
import { ColdStartPanel } from "./components/ColdStart";
import { RollCallPanel } from "./components/RollCall";
import { ProjectsColumn } from "./components/projects";
import type { ProjectSort } from "./components/projects";
import { SearchLauncher, SearchSheet, hasPrimaryModifier } from "./components/search";
import { SessionsColumn } from "./components/sessions";
import type { SessionFacts } from "./components/sessions";
import { ThemeToggle } from "./components/ThemeToggle";
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
import { desktopHost } from "./lib/desktop-host";
import { compareOrderedSessions } from "./lib/activity-order";
import { useActivityOrder } from "./lib/use-activity-order";
import { client } from "./lib/rosetta";
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

function safeStorage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

export default function App() {
  const { meta, snapshotVersion, status: metaStatus } = useMeta();
  const nowMs = useNow(meta);
  const daemonStatus = useDaemonStatus();
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
  const [query, setQuery] = useState(
    () => new URLSearchParams(window.location.search).get("q") ?? "",
  );
  const [sheetOpen, setSheetOpen] = useState(false);
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
  const search = useSearch(query, {
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
  const stats = useProjectStats(projectIds, canonicalReady === true);

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
    const fromShell = desktopHost()?.homeDir ?? null;
    if (fromShell !== null) return fromShell;
    const root = meta?.runtimes[0]?.data_root ?? null;
    if (root === null || !root.startsWith("/")) return null;
    return root.replace(/\/\.[^/]+$/, "");
  }, [meta]);

  const projectViewModels: ProjectViewModel[] = useMemo(
    () =>
      projectRows.map((p) =>
        buildProjectViewModel(p, meta?.sessions ?? [], stats.get(p.id), kindOf, nowMs),
      ),
    [projectRows, meta, stats, kindOf, nowMs],
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
    client.boost(viewedSessionId).catch(() => {});
  }, [viewedSessionId, sessionState, ingest?.phase]);
  const feed = useTimeline(viewedSessionId);
  const usage = useProjectUsage(projectId, canonicalReady === true);

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
  const worktree = useWorktreeGuest(projectRoot);

  /* ── search: one view model, held over the room ─────────────────────── */
  const searchResults = useMemo(() => {
    if (search.query === "" || search.response === null || meta === null) return null;
    return createSearchResultsViewModel(search.response.hits, meta.sessions, projectById);
  }, [search.query, search.response, meta, projectById]);

  // Opening a hit is a continuation, not a navigation: the sheet leaves, the
  // room opens the thread, the transcript lands on the message, and the
  // stepper remembers the session's other matches.
  const openHit = (rootSession: SessionRow, hit: SearchHit) => {
    rememberQuery(safeStorage(), query);
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
        const hit = current.hits[at];
        setInspectedThread(null);
        setFocus({ sessionId: hit.session_id, timelineId: hit.timeline_id });
        return { ...current, at };
      });
    },
    [],
  );

  // The stepper belongs to the session it was born in.
  useEffect(() => {
    if (stepper !== null && stepper.sessionId !== sessionId) setStepper(null);
  }, [sessionId, stepper]);

  const clearSearch = () => {
    setQuery("");
    setStepper(null);
  };

  // ⌘K summons the sheet from anywhere; `/` does the same when nothing is
  // being typed; ⌘G / ⇧⌘G walk the landed session's matches.
  const stepping = stepper !== null;
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.altKey) return;
      const primary = hasPrimaryModifier(event);
      if (primary && (event.key === "k" || event.key === "K")) {
        event.preventDefault();
        setSheetOpen((open) => !open);
        return;
      }
      if (primary && stepping && !sheetOpen && (event.key === "g" || event.key === "G")) {
        event.preventDefault();
        stepMatch(event.shiftKey ? -1 : 1);
        return;
      }
      if (
        event.key === "/" &&
        !primary &&
        !sheetOpen &&
        !isTypingTarget(event.target) &&
        !document.body.querySelector("[data-ah-fileview]")
      ) {
        event.preventDefault();
        setSheetOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sheetOpen, stepping, stepMatch]);

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
    <div className="ah-frame is-tall">
      <div className="ah-room">
        <div className="ah-searchbar">
          <SearchLauncher
            query={search.query}
            placeholder={launcherPlaceholder}
            onOpen={() => setSheetOpen(true)}
            onClear={clearSearch}
          />
          <ThemeToggle />
        </div>
        {disconnected && !coldStarting ? (
          <EmptyState
            style={{ flex: 1 }}
            title={
              metaStatus === "closed"
                ? "rosettad is not answering"
                : "Connecting to rosettad"
            }
            description={connectionError ?? (
              <>
                Looking for the daemon at <code>{client.baseUrl}</code>. Start it with{" "}
                <code>rosettad serve --db &lt;projection&gt;</code> and this room fills in on
                its own.
              </>
            )}
          >
            <div className="ah-meter">
              <i style={{ width: metaStatus === "connecting" ? "30%" : "8%" }} />
            </div>
          </EmptyState>
        ) : awaitingConsent ? (
          <RollCallPanel />
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
      <SearchSheet
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
      />
      {receipt !== null && <div className="ah-receipt">{receipt}</div>}
    </div>
  );
}
