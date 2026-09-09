import type { SearchHit, SessionRow } from "@rosetta/protocol";
import type { ProjectViewModel } from "./model.ts";
import { projectKey } from "./model.ts";

export interface SessionSearchResult {
  readonly session: SessionRow;
  readonly hits: readonly SearchHit[];
}

export interface ProjectSearchResultGroup {
  readonly project: ProjectViewModel;
  readonly sessions: readonly SessionSearchResult[];
  readonly hitCount: number;
}

export interface SearchResultCounts {
  readonly matches: number;
  readonly sessions: number;
  readonly projects: number;
}

export interface SearchResultsViewModel {
  readonly groups: readonly ProjectSearchResultGroup[];
  readonly hitCountByProject: ReadonlyMap<string, number>;
  readonly counts: SearchResultCounts;
}

function compareSessionResults(
  left: SessionSearchResult,
  right: SessionSearchResult,
): number {
  return (
    right.hits.length - left.hits.length ||
    right.session.updated_at - left.session.updated_at ||
    left.session.id.localeCompare(right.session.id)
  );
}

function compareProjectGroups(
  left: ProjectSearchResultGroup,
  right: ProjectSearchResultGroup,
): number {
  return (
    right.hitCount - left.hitCount ||
    left.project.row.name.localeCompare(right.project.row.name) ||
    left.project.row.id.localeCompare(right.project.row.id)
  );
}

/** Resolve navigation from the authoritative parent chain. `root_session_id`
 * remains a useful fallback for an orphaned retained child, but an immediate
 * parent must never masquerade as a top-level thread for a nested hit. */
function navigationRoot(
  session: SessionRow,
  sessions: ReadonlyMap<string, SessionRow>,
): SessionRow | null {
  let current = session;
  const visited = new Set<string>();
  while (current.parent_session_id !== null && !visited.has(current.id)) {
    visited.add(current.id);
    const parent = sessions.get(current.parent_session_id);
    if (!parent) return sessions.get(session.root_session_id) ?? null;
    current = parent;
  }
  return current.parent_session_id === null ? current : null;
}

/**
 * Groups protocol search hits once for every UI consumer. A child-session hit
 * belongs to its root thread in navigation; the hit keeps its own session id
 * so opening it can expand and land inside the embedded subthread.
 */
export function createSearchResultsViewModel(
  hits: readonly SearchHit[],
  sessions: readonly SessionRow[],
  projectsById: ReadonlyMap<string, ProjectViewModel>,
): SearchResultsViewModel {
  const sessionById = new Map(sessions.map((session) => [session.id, session]));
  const hitsBySessionId = new Map<string, SearchHit[]>();

  for (const hit of hits) {
    const hitSession = sessionById.get(hit.session_id);
    if (!hitSession) continue;
    const rootSession = navigationRoot(hitSession, sessionById);
    if (!rootSession) continue;
    const sessionHits = hitsBySessionId.get(rootSession.id);
    if (sessionHits) sessionHits.push(hit);
    else hitsBySessionId.set(rootSession.id, [hit]);
  }

  const sessionsByProjectId = new Map<string, SessionSearchResult[]>();
  for (const [sessionId, sessionHits] of hitsBySessionId) {
    const session = sessionById.get(sessionId);
    if (!session) continue;

    const projectId = projectKey(session);
    if (!projectsById.has(projectId)) continue;

    const projectSessions = sessionsByProjectId.get(projectId);
    const result = { session, hits: sessionHits };
    if (projectSessions) projectSessions.push(result);
    else sessionsByProjectId.set(projectId, [result]);
  }

  const groups: ProjectSearchResultGroup[] = [];
  for (const [projectId, projectSessions] of sessionsByProjectId) {
    const project = projectsById.get(projectId);
    if (!project) continue;

    const sortedSessions = projectSessions.sort(compareSessionResults);
    groups.push({
      project,
      sessions: sortedSessions,
      hitCount: sortedSessions.reduce(
        (total, result) => total + result.hits.length,
        0,
      ),
    });
  }
  groups.sort(compareProjectGroups);

  const hitCountByProject = new Map(
    groups.map((group) => [group.project.row.id, group.hitCount]),
  );
  const counts: SearchResultCounts = {
    matches: groups.reduce((total, group) => total + group.hitCount, 0),
    sessions: groups.reduce((total, group) => total + group.sessions.length, 0),
    projects: groups.length,
  };

  return { groups, hitCountByProject, counts };
}
