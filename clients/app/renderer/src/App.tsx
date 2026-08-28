// The History room. Columns change content, never geometry (H-2); search is
// the front door and transforms columns 2–3; the ledger's scale is global
// and stable whatever the filter says. The worktree rail is a guest and the
// measure is not: below 1360px the rail closes rather than squeezing the
// transcript (H-3), and an explicit toggle outranks the automatic close.
import { useEffect, useMemo, useRef, useState } from "react";
import type { SearchHit, SearchReadiness, SessionRow } from "@rosetta/protocol";
import { LedgerColumn } from "./components/Ledger";
import type { LedgerSort } from "./components/Ledger";
import { RailColumn } from "./components/Rail";
import { SearchStrip } from "./components/SearchStrip";
import { SessionsColumn } from "./components/Sessions";
import type { SessionFacts } from "./components/Sessions";
import { TranscriptColumn } from "./components/Transcript";
import { useMeta, useProjectStats, useProjectUsage, useSearch, useTimeline } from "./lib/hooks";
import { buildProjectVM, yearGridStartMs } from "./lib/model";
import type { ProjectVM } from "./lib/model";
import { host } from "./lib/rail-port";
import { client } from "./lib/rosetta";

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
  const [sort, setSort] = useState<LedgerSort>("spend");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [focusId, setFocusId] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<string | null>(null);

  const search = useSearch(query);
  const searching = search.query !== "";

  const projectIds = useMemo(() => (meta?.projects ?? []).map((p) => p.id).sort(), [meta]);
  const stats = useProjectStats(projectIds);

  const kindOf = useMemo(() => {
    const byId = new Map((meta?.runtimes ?? []).map((r) => [r.id, r.kind]));
    return (runtimeId: string) => byId.get(runtimeId) ?? runtimeId;
  }, [meta]);

  const home = useMemo(() => {
    const root = meta?.runtimes[0]?.data_root ?? null;
    return root ? root.replace(/\/\.[^/]+$/, "") : null;
  }, [meta]);

  const vms: ProjectVM[] = useMemo(
    () =>
      (meta?.projects ?? []).map((p) =>
        buildProjectVM(p, meta?.sessions ?? [], stats.get(p.id), kindOf, nowMs),
      ),
    [meta, stats, kindOf, nowMs],
  );
  const vmById = useMemo(() => new Map(vms.map((vm) => [vm.row.id, vm])), [vms]);

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
    if (vms.length === 0 || stats.size === 0) return;
    if (projectId === null || !vmById.has(projectId)) {
      const top = [...vms].sort((a, b) => b.tokens30 - a.tokens30)[0];
      setProjectId(top.row.id);
    }
  }, [vms, vmById, projectId, stats]);

  const projectSessions = useMemo(() => {
    if (projectId === null) return [];
    return (meta?.sessions ?? [])
      .filter((s) => s.project_id === projectId && s.id === s.root_session_id)
      .sort((a, b) => b.updated_at - a.updated_at);
  }, [meta, projectId]);

  useEffect(() => {
    if (projectSessions.length === 0) return;
    if (sessionId === null || !projectSessions.some((s) => s.id === sessionId)) {
      setSessionId(projectSessions[0].id);
    }
  }, [projectSessions, sessionId]);

  const session = useMemo(
    () => (meta?.sessions ?? []).find((s) => s.id === sessionId) ?? null,
    [meta, sessionId],
  );
  const feed = useTimeline(sessionId);
  const usage = useProjectUsage(projectId);

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
  const railShown = railAvailable && railWanted && (railChosen || frameWide);
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

  const projectRoot = useMemo(() => {
    if (projectId === null) return null;
    return vmById.get(projectId)?.row.root_paths[0] ?? null;
  }, [projectId, vmById]);

  // Point the engine at the selected project's root; kill-and-refork is the
  // host's concern, so this only speaks when the root actually changes.
  const openedRoot = useRef<string | null>(null);
  useEffect(() => {
    if (!railShown || projectRoot === null) return;
    if (openedRoot.current === projectRoot) return;
    openedRoot.current = projectRoot;
    void host()?.openRail(projectRoot);
  }, [railShown, projectRoot]);

  /* ── search groupings: project → session → hits ─────────────────────── */
  const grouped = useMemo(() => {
    if (!searching || search.response === null || meta === null) return null;
    const sessionById = new Map(meta.sessions.map((s) => [s.id, s]));
    const bySession = new Map<string, SearchHit[]>();
    for (const hit of search.response.hits) {
      const list = bySession.get(hit.session_id);
      if (list) list.push(hit);
      else bySession.set(hit.session_id, [hit]);
    }
    const byProject = new Map<string, { session: SessionRow; hits: SearchHit[] }[]>();
    for (const [sid, hits] of bySession) {
      const s = sessionById.get(sid);
      if (!s) continue;
      const pid = s.project_id ?? "";
      const list = byProject.get(pid);
      const entry = { session: s, hits };
      if (list) list.push(entry);
      else byProject.set(pid, [entry]);
    }
    const groups = [...byProject.entries()]
      .map(([pid, entries]) => ({
        project: vmById.get(pid) ?? null,
        entries: entries.sort((a, b) => b.hits.length - a.hits.length),
      }))
      .filter((g): g is { project: ProjectVM; entries: typeof g.entries } => g.project !== null)
      .sort(
        (a, b) =>
          b.entries.reduce((n, e) => n + e.hits.length, 0) -
          a.entries.reduce((n, e) => n + e.hits.length, 0),
      );
    return groups;
  }, [searching, search.response, meta, vmById]);

  const ledgerCounts = useMemo(() => {
    if (grouped === null) return null;
    const out = new Map<string, number>();
    for (const g of grouped) {
      out.set(
        g.project.row.id,
        g.entries.reduce((n, e) => n + e.hits.length, 0),
      );
    }
    return out;
  }, [grouped]);

  const searchCounts = useMemo(() => {
    if (grouped === null) return null;
    return {
      matches: grouped.reduce((n, g) => n + g.entries.reduce((m, e) => m + e.hits.length, 0), 0),
      sessions: grouped.reduce((n, g) => n + g.entries.length, 0),
      projects: grouped.length,
    };
  }, [grouped]);

  const openHit = (hitSession: SessionRow, hit: SearchHit) => {
    setQuery("");
    if (hitSession.project_id) setProjectId(hitSession.project_id);
    setSessionId(hitSession.root_session_id);
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
          <div className="ah-empty" style={{ flex: 1 }}>
            <div className="ah-meter">
              <i style={{ width: metaStatus === "connecting" ? "30%" : "8%" }} />
            </div>
            <strong>
              {metaStatus === "closed" ? "rosettad is not answering" : "Connecting to rosettad"}
            </strong>
            <p>
              Looking for the daemon at <code>{client.baseUrl}</code>. Start it with{" "}
              <code>rosettad serve --db &lt;projection&gt;</code> and this room fills in on its own.
            </p>
          </div>
        ) : (
          <div
            className="ah-cols"
            data-mode={searching ? "search" : "browse"}
            data-rail={railShown ? "on" : "off"}
          >
            <LedgerColumn
              vms={vms}
              activeId={projectId}
              sort={sort}
              onSort={setSort}
              hitCounts={ledgerCounts}
              home={home}
              nowMs={nowMs}
              onSelect={(id) => {
                setProjectId(id);
                setSessionId(null);
              }}
            />
            <SessionsColumn
              project={projectId ? (vmById.get(projectId) ?? null) : null}
              sessions={projectSessions}
              facts={facts}
              kindOf={kindOf}
              activeId={sessionId}
              usage={usage}
              gridStartMs={yearGridStartMs(nowMs)}
              nowMs={nowMs}
              railOn={railAvailable ? railShown : null}
              onToggleRail={toggleRail}
              onSelect={(s) => {
                if (searching) {
                  setQuery("");
                  if (s.project_id) setProjectId(s.project_id);
                }
                setSessionId(s.id);
              }}
              searchGroups={
                grouped === null
                  ? null
                  : grouped.map((g) => ({
                      project: g.project,
                      sessions: g.entries.map((e) => ({ session: e.session, hits: e.hits.length })),
                    }))
              }
            />
            {/* Always in the grid — a closed rail is a 0px track, and the
                measure takes back whatever the guest gives up. */}
            <RailColumn
              projectRoot={projectRoot}
              tint={projectId ? (vmById.get(projectId)?.tint ?? null) : null}
              home={home}
            />
            <TranscriptColumn
              session={session}
              project={session?.project_id ? (vmById.get(session.project_id) ?? null) : null}
              feed={feed}
              focusId={focusId}
              onFocusConsumed={() => setFocusId(null)}
              onReceipt={showReceipt}
              searchHits={
                grouped === null
                  ? null
                  : grouped.flatMap((g) =>
                      g.entries.map((e) => ({ project: g.project, session: e.session, hits: e.hits })),
                    )
              }
              searchCounts={searchCounts}
              onOpenHit={openHit}
            />
          </div>
        )}
      </div>
      {receipt !== null && <div className="ah-receipt">{receipt}</div>}
    </div>
  );
}
