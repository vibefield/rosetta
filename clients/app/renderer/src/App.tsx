// The History room. Columns change content, never geometry (H-2); search is
// the front door and transforms columns 2–3; the ledger's scale is global
// and stable whatever the filter says. The worktree rail is a guest and the
// measure is not: below 1360px the rail closes rather than squeezing the
// transcript (H-3), and an explicit toggle outranks the automatic close.
import { useEffect, useMemo, useRef, useState } from "react";
import type { SearchHit, SearchReadiness, SessionRow } from "@rosetta/protocol";
import { ProjectsColumn } from "./components/projects";
import type { ProjectSort } from "./components/projects";
import { RailColumn } from "./components/Rail";
import { SearchStrip } from "./components/SearchStrip";
import { SessionsColumn } from "./components/sessions";
import type { SessionFacts, SessionScope } from "./components/sessions";
import { TranscriptColumn } from "./components/Transcript";
import { EmptyState } from "./components/ui";
import { useMeta, useProjectStats, useProjectUsage, useSearch, useTimeline } from "./lib/hooks";
import {
  UNASSIGNED_PROJECT_ID,
  buildProjectViewModel,
  getActivityGridStartTimestamp,
  projectKey,
  unassignedProject,
} from "./lib/model";
import type { ProjectViewModel } from "./lib/model";
import { host } from "./lib/rail-port";
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

function useSearchReadiness(): SearchReadiness | null {
  const [readiness, setReadiness] = useState<SearchReadiness | null>(null);
  useEffect(() => {
    let cancelled = false;
    const poll = () => {
      client
        .status()
        .then((s) => {
          if (!cancelled) setReadiness(s.search.readiness);
        })
        .catch(() => {
          if (!cancelled) setReadiness(null);
        });
    };
    poll();
    const t = setInterval(poll, 30_000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  }, []);
  return readiness;
}

export default function App() {
  const nowMs = useNow();
  const { meta, status: metaStatus } = useMeta();
  const readiness = useSearchReadiness();
  const [query, setQuery] = useState(
    () => new URLSearchParams(window.location.search).get("q") ?? "",
  );
  const [sort, setSort] = useState<ProjectSort>("spend");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [sessionScope, setSessionScope] = useState<SessionScope>("all");
  const [focusId, setFocusId] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<string | null>(null);

  const search = useSearch(query);
  const searching = search.query !== "";

  const projectIds = useMemo(() => (meta?.projects ?? []).map((p) => p.id).sort(), [meta]);
  const stats = useProjectStats(projectIds);

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
    const fromShell = host()?.homeDir ?? null;
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

  // Default selections once data lands; heal them when they go stale. The
  // default is the top 30d spender, so it waits for the figures — picking
  // before stats land would crown whichever project happened to list first.
  useEffect(() => {
    if (
      projectViewModels.length === 0 ||
      (projectIds.length > 0 && stats.size === 0)
    ) {
      return;
    }
    if (projectId === null || !projectById.has(projectId)) {
      const top = [...projectViewModels].sort(
        (a, b) => b.tokensLast30Days - a.tokensLast30Days,
      )[0];
      setProjectId(top.row.id);
    }
  }, [projectViewModels, projectById, projectId, stats, projectIds.length]);

  const projectSessions = useMemo(() => {
    if (projectId === null) return [];
    return (meta?.sessions ?? [])
      .filter(
        (s) =>
          projectKey(s) === projectId &&
          (sessionScope === "all" || s.id === s.root_session_id),
      )
      .sort((a, b) => b.updated_at - a.updated_at);
  }, [meta, projectId, sessionScope]);

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
  const feed = useTimeline(sessionId);
  const usage = useProjectUsage(projectId === UNASSIGNED_PROJECT_ID ? null : projectId);

  const projectRoot = useMemo(() => {
    if (projectId === null) return null;
    return projectById.get(projectId)?.row.root_paths[0] ?? null;
  }, [projectId, projectById]);

  /* ── the worktree rail: a guest, per H-3 ────────────────────────────── */
  const railAvailable = host() !== null;
  const [railWanted, setRailWanted] = useState(true);
  // An explicit choice outranks the automatic close — once the reader
  // presses the toggle, the width heuristic stops applying.
  const [railChosen, setRailChosen] = useState(false);
  const [frameWide, setFrameWide] = useState(() => window.innerWidth >= 1360);
  useEffect(() => {
    const measure = () => setFrameWide(window.innerWidth >= 1360);
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, []);
  const railShown =
    railAvailable && projectRoot !== null && railWanted && (railChosen || frameWide);
  // The toggle flips what is SHOWN, not what was last wanted — the first
  // press on an already-auto-closed rail must never do nothing.
  const toggleRail = () => {
    if (railShown) {
      setRailWanted(false);
    } else {
      setRailWanted(true);
      setRailChosen(true);
    }
  };

  // Point the engine at the selected project's root; kill-and-refork is the
  // host's concern, so this only speaks when the root actually changes.
  const openedRoot = useRef<string | null>(null);
  useEffect(() => {
    if (!railShown || projectRoot === null) return;
    if (openedRoot.current === projectRoot) return;
    openedRoot.current = projectRoot;
    void host()?.openRail(projectRoot);
  }, [railShown, projectRoot]);

  /* ── one search view model shared by all three result columns ───────── */
  const searchResults = useMemo(() => {
    if (!searching || search.response === null || meta === null) return null;
    return createSearchResultsViewModel(
      search.response.hits,
      meta.sessions,
      projectById,
    );
  }, [searching, search.response, meta, projectById]);

  const openHit = (hitSession: SessionRow, hit: SearchHit) => {
    setQuery("");
    setProjectId(projectKey(hitSession));
    setSessionScope("all");
    setSessionId(hitSession.id);
    setFocusId(hit.timeline_id);
  };

  const showReceipt = (text: string) => {
    setReceipt(text);
    window.setTimeout(() => setReceipt(null), 2600);
  };

  const kinds = (meta?.runtimes ?? []).map((r) => r.kind);
  const scope = search.error
    ? `search: ${search.error}`
    : [
        "your whole agentic past",
        ...kinds,
        ...(readiness && readiness !== "ready" ? [`index ${readiness}`] : []),
      ].join(" · ");

  const disconnected = meta === null;

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
        ) : (
          <div
            className="ah-cols"
            data-mode={searching ? "search" : "browse"}
            data-rail={railShown ? "on" : "off"}
          >
            <ProjectsColumn
              projects={projectViewModels}
              selectedProjectId={projectId}
              sort={sort}
              onSortChange={setSort}
              searchHitCounts={searchResults?.hitCountByProject ?? null}
              homeDirectory={home}
              currentTimestamp={nowMs}
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
              sessionScope={sessionScope}
              onSessionScopeChange={setSessionScope}
              activityGridStartTimestamp={getActivityGridStartTimestamp(nowMs)}
              currentTimestamp={nowMs}
              isWorktreeVisible={
                railAvailable && projectRoot !== null ? railShown : null
              }
              onToggleWorktree={toggleRail}
              onSessionSelect={(selectedSession) => {
                if (searching) {
                  setQuery("");
                  setProjectId(projectKey(selectedSession));
                  setSessionScope("all");
                }
                setSessionId(selectedSession.id);
              }}
              searchGroups={searchResults?.groups ?? null}
            />
            {/* Always in the grid — a closed rail is a 0px track, and the
                measure takes back whatever the guest gives up. */}
            <RailColumn
              projectRoot={projectRoot}
              tint={projectId ? (projectById.get(projectId)?.tint ?? null) : null}
              home={home}
            />
            <TranscriptColumn
              session={session}
              project={session ? (projectById.get(projectKey(session)) ?? null) : null}
              feed={feed}
              focusId={focusId}
              onFocusConsumed={() => setFocusId(null)}
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
