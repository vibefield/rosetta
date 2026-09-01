// The History room. Columns change content, never geometry (H-2); search is
// the front door and transforms columns 2–3; the ledger's scale is global
// and stable whatever the filter says. The worktree column is a guest and
// the measure is not (H-3) — see `components/worktree/use-worktree-guest`,
// which owns that rule and the engine lifecycle behind it.
import { useEffect, useMemo, useState } from "react";
import type { SearchHit, SessionRow } from "@rosetta/protocol";
import { ColdStartPanel } from "./components/ColdStart";
import { RollCallPanel } from "./components/RollCall";
import { ProjectsColumn } from "./components/projects";
import type { ProjectSort } from "./components/projects";
import { SearchStrip } from "./components/SearchStrip";
import { SessionsColumn } from "./components/sessions";
import type { SessionFacts } from "./components/sessions";
import { TranscriptColumn } from "./components/Transcript";
import type { ThreadFocus } from "./components/Transcript";
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
import { backfillProgress, isIngestActive } from "./lib/ingest";
import {
  UNASSIGNED_PROJECT_ID,
  buildProjectViewModel,
  getActivityGridStartTimestamp,
  projectKey,
  unassignedProject,
} from "./lib/model";
import type { ProjectViewModel } from "./lib/model";
import { desktopHost } from "./lib/desktop-host";
import { client } from "./lib/rosetta";
import { createSearchResultsViewModel } from "./lib/search-results";

function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  return now;
}

export default function App() {
  const nowMs = useNow();
  const { meta, status: metaStatus } = useMeta();
  const { canonicalReady, searchReadiness: readiness, ingest } = useDaemonStatus();
  const ingestActive = isIngestActive(ingest);
  const [query, setQuery] = useState(
    () => new URLSearchParams(window.location.search).get("q") ?? "",
  );
  const [sort, setSort] = useState<ProjectSort>("recent");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [focus, setFocus] = useState<ThreadFocus | null>(null);
  const [receipt, setReceipt] = useState<string | null>(null);

  const search = useSearch(query);
  const searching = search.query !== "";

  const projectIds = useMemo(() => (meta?.projects ?? []).map((p) => p.id).sort(), [meta]);
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
        out.set(row.session_id, { tokens: row.tokens, messages: row.messages });
      }
    }
    return out;
  }, [stats]);

  // Default selections once data lands; heal them when they go stale. Match
  // the project's initial selection to the list's recent-first ranking.
  useEffect(() => {
    if (projectViewModels.length === 0) return;
    if (projectId === null || !projectById.has(projectId)) {
      const top = [...projectViewModels].sort(
        (a, b) => (b.lastActiveMs ?? 0) - (a.lastActiveMs ?? 0),
      )[0];
      setProjectId(top.row.id);
    }
  }, [projectViewModels, projectById, projectId]);

  const projectSessions = useMemo(() => {
    if (projectId === null) return [];
    return (meta?.sessions ?? [])
      .filter(
        (s) =>
          projectKey(s) === projectId &&
          s.parent_session_id === null,
      )
      .sort((a, b) => b.updated_at - a.updated_at);
  }, [meta, projectId]);

  useEffect(() => {
    if (projectSessions.length === 0) {
      setSessionId(null);
      return;
    }
    if (sessionId === null || !projectSessions.some((s) => s.id === sessionId)) {
      setSessionId(projectSessions[0].id);
    }
  }, [projectSessions, sessionId]);

  const session = useMemo(
    () => (meta?.sessions ?? []).find((s) => s.id === sessionId) ?? null,
    [meta, sessionId],
  );
  // Attention-directed ingestion: opening a not-yet-indexed session moves
  // its queued work to the front of the backfill queue. Under slicing the
  // common case is "streaming" — the starter landed and the completion
  // remainder is still queued — and that remainder is exactly what the
  // click promotes; "pending" covers a session no pass has reached at all.
  // Advisory — a session with nothing queued is a no-op, an old daemon just
  // keeps its order, and the ledger dedupes repeats.
  const sessionState = session?.stream_state ?? null;
  useEffect(() => {
    if (sessionId === null || sessionState === null || sessionState === "indexed") return;
    if (ingest?.phase !== "backfilling") return;
    client.boost(sessionId).catch(() => {});
  }, [sessionId, sessionState, ingest?.phase]);
  const feed = useTimeline(sessionId);
  const usage = useProjectUsage(
    projectId === UNASSIGNED_PROJECT_ID ? null : projectId,
    canonicalReady === true,
  );

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

  /* ── one search view model shared by all three result columns ───────── */
  const searchResults = useMemo(() => {
    if (!searching || search.response === null || meta === null) return null;
    return createSearchResultsViewModel(
      search.response.hits,
      meta.sessions,
      projectById,
    );
  }, [searching, search.response, meta, projectById]);

  const openHit = (rootSession: SessionRow, hit: SearchHit) => {
    setQuery("");
    setProjectId(projectKey(rootSession));
    setSessionId(rootSession.id);
    setFocus({ sessionId: hit.session_id, timelineId: hit.timeline_id });
  };

  const showReceipt = (text: string) => {
    setReceipt(text);
    window.setTimeout(() => setReceipt(null), 2600);
  };

  const kinds = (meta?.runtimes ?? []).map((r) => r.kind);
  const warming = backfillProgress(ingest);
  const scope = search.error
    ? `search: ${search.error}`
    : [
        "your whole agentic past",
        ...kinds,
        // "warming" belongs to the backfill alone — the roll call's wait
        // heats nothing.
        ...(ingest?.phase === "backfilling"
          ? [`warming${warming !== null ? ` ${Math.round(warming * 100)}%` : ""}`]
          : []),
        ...(ingest?.phase === "metadata_ready" ? ["metadata audit · transcripts paused"] : []),
        // A fresh install has no index yet — that is warming, not failure;
        // "index failed" is reserved for a live daemon whose index broke.
        ...(readiness && readiness !== "ready" && !(ingestActive && readiness === "failed")
          ? [`index ${readiness}`]
          : []),
      ].join(" · ");

  const disconnected = meta === null;
  // The roll call: discovery is on the board and nothing has been read.
  const awaitingConsent = ingest?.phase === "awaiting_consent";
  // The first run before its first metadata batch: the daemon answers and
  // meters move, but there is not yet a canonical project to render. Once
  // RAM metadata arrives the ordinary room takes over progressively.
  const coldStarting =
    !disconnected && projectRows.length === 0 && ingestActive && ingest !== null;

  return (
    <div className="ah-frame is-tall">
      <div className="ah-room">
        <SearchStrip query={query} onQuery={setQuery} scope={scope} />
        {disconnected ? (
          <EmptyState
            style={{ flex: 1 }}
            title={
              metaStatus === "closed"
                ? "rosettad is not answering"
                : "Connecting to rosettad"
            }
            description={
              <>
                Looking for the daemon at <code>{client.baseUrl}</code>. Start it with{" "}
                <code>rosettad serve --db &lt;projection&gt;</code> and this room fills in on
                its own.
              </>
            }
          >
            <div className="ah-meter">
              <i style={{ width: metaStatus === "connecting" ? "30%" : "8%" }} />
            </div>
          </EmptyState>
        ) : awaitingConsent ? (
          <RollCallPanel />
        ) : coldStarting ? (
          <ColdStartPanel ingest={ingest} />
        ) : (
          <div
            className="ah-cols"
            data-mode={searching ? "search" : "browse"}
            data-rail={worktree.visible ? "on" : "off"}
          >
            <ProjectsColumn
              projects={projectViewModels}
              selectedProjectId={projectId}
              sort={sort}
              onSortChange={setSort}
              searchHitCounts={searchResults?.hitCountByProject ?? null}
              homeDirectory={home}
              currentTimestamp={nowMs}
              ingest={ingest}
              onProjectSelect={(id) => {
                setProjectId(id);
                setSessionId(null);
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
              onSessionSelect={(selectedSession) => {
                if (searching) {
                  setQuery("");
                  setProjectId(projectKey(selectedSession));
                }
                setSessionId(selectedSession.id);
              }}
              searchGroups={searchResults?.groups ?? null}
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
              searchResults={searchResults}
              onOpenHit={openHit}
            />
          </div>
        )}
      </div>
      {receipt !== null && <div className="ah-receipt">{receipt}</div>}
    </div>
  );
}
