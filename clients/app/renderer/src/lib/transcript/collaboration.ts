// Delegated-work presentation facts.
//
// The canonical timeline deliberately stores evidence, not UI geometry. This
// module is the small pure seam that turns proven Agent Team relations into a
// shared chronology and roster without teaching React about Claude envelopes.
// Ordinary Agent workers never enter these helpers: they remain invocation-
// rooted child documents.
import type { ParticipantColor, SessionRow, TimelineEntry } from "@rosetta/protocol";
import type { LedgerLine } from "./ledger.ts";
import { linkedChildSessionIds } from "./ledger.ts";
import { canonicalText } from "./vocabulary.ts";

export type RuntimeProfileBasis = "observed" | "varies" | "configured" | "unknown";

export interface RuntimeProfile {
  readonly basis: RuntimeProfileBasis;
  readonly label: string;
  readonly model: string | null;
  readonly effort: string | null;
  readonly profileCount: number;
}

export type TeamMemberOperation = "start" | "add" | "resume";

export interface TeamMemberPresentation {
  readonly row: TimelineEntry;
  readonly session: SessionRow | null;
  readonly operation: TeamMemberOperation;
  readonly team: string;
  readonly name: string;
  readonly color: ParticipantColor | null;
  readonly objective: string;
  readonly configuredModel: string | null;
  readonly runtime: RuntimeProfile;
}

export interface TeamParticipantPresentation {
  readonly color: ParticipantColor | null;
  readonly runtime: RuntimeProfile;
  readonly sessionId: string | null;
}

export interface WorkerThreadLinks {
  /** The one durable disclosure that owns each ordinary worker document. */
  readonly anchorRowByChild: ReadonlyMap<string, string>;
  /** Children mounted below their earliest proven invocation. */
  readonly childrenByRow: ReadonlyMap<string, readonly SessionRow[]>;
  /** Later delegate/resume lines which navigate back to that disclosure. */
  readonly referencedChildIdsByRow: ReadonlyMap<string, readonly string[]>;
}

export interface TeamReportWork {
  readonly rows: readonly TimelineEntry[];
  readonly actionCount: number;
  readonly startedAt: number | null;
  readonly endedAt: number | null;
}

const UNKNOWN_RUNTIME: RuntimeProfile = {
  basis: "unknown",
  label: "runtime not recorded",
  model: null,
  effort: null,
  profileCount: 0,
};

/** Response evidence wins over configuration. Split rows in one response are
 * first joined by response id, then distinct model/effort pairs are counted. */
export function runtimeProfile(
  rows: readonly TimelineEntry[],
  configuredModel: string | null,
): RuntimeProfile {
  const responses = new Map<string, { model: string | null; effort: string | null }>();
  for (const row of rows) {
    if (row.response === null || (row.model === null && row.effort === null)) continue;
    const current = responses.get(row.response.id) ?? { model: null, effort: null };
    responses.set(row.response.id, {
      model: current.model ?? row.model,
      effort: current.effort ?? row.effort,
    });
  }

  const profiles = [
    ...new Map(
      [...responses.values()].map((profile) => [
        `${profile.model ?? ""}\u0000${profile.effort ?? ""}`,
        profile,
      ]),
    ).values(),
  ];
  if (profiles.length > 1) {
    return {
      basis: "varies",
      label: `varies · ${profiles.length} response profiles`,
      model: null,
      effort: null,
      profileCount: profiles.length,
    };
  }
  const observed = profiles[0];
  if (observed !== undefined) {
    const model = observed.model ?? "model unknown";
    const effort = observed.effort === null ? "effort unknown" : `${observed.effort} effort`;
    return {
      basis: "observed",
      label: `${model} · ${effort}`,
      model: observed.model,
      effort: observed.effort,
      profileCount: 1,
    };
  }
  if (configuredModel !== null) {
    return {
      basis: "configured",
      label: `configured ${configuredModel} · effort unknown`,
      model: configuredModel,
      effort: null,
      profileCount: 0,
    };
  }
  return UNKNOWN_RUNTIME;
}

/** Direct child sessions that native evidence identifies as team members. */
export function teamSessionIds(
  parentSessionId: string,
  directChildren: readonly SessionRow[],
  parentRows: readonly TimelineEntry[],
): readonly string[] {
  const direct = new Map(
    directChildren
      .filter((child) => child.parent_session_id === parentSessionId)
      .map((child) => [child.id, child]),
  );
  const ids = new Set(
    directChildren
      .filter(
        (child) => child.parent_session_id === parentSessionId && child.role === "team",
      )
      .map((child) => child.id),
  );
  for (const row of parentRows) {
    if (row.body.type !== "tool_use") continue;
    const action = row.body.tool.action;
    if (
      action.type !== "delegate" ||
      action.team === null ||
      action.child_session_id === null ||
      !direct.has(action.child_session_id)
    ) {
      continue;
    }
    ids.add(action.child_session_id);
  }
  return [...ids].sort();
}

/** Roster facts and the start/add/resume operation for every proven team
 * delegate row. The first observed member starts the team; subsequent rows add
 * members unless the native request says resume. */
export function teamMemberPresentations(
  rows: readonly TimelineEntry[],
  sessions: ReadonlyMap<string, SessionRow>,
  rowsBySession: ReadonlyMap<string, readonly TimelineEntry[]>,
): ReadonlyMap<string, TeamMemberPresentation> {
  const out = new Map<string, TeamMemberPresentation>();
  const seenTeams = new Set<string>();
  for (const row of rows) {
    if (row.body.type !== "tool_use") continue;
    const action = row.body.tool.action;
    if (action.type !== "delegate" || action.team === null) continue;
    const session =
      action.child_session_id === null ? null : (sessions.get(action.child_session_id) ?? null);
    const operation: TeamMemberOperation =
      action.resume_id !== null ? "resume" : seenTeams.has(action.team) ? "add" : "start";
    seenTeams.add(action.team);
    const name =
      action.name ?? session?.title ?? action.agent_kind ?? session?.native_session_id ?? "teammate";
    const memberRows =
      action.child_session_id === null ? [] : (rowsBySession.get(action.child_session_id) ?? []);
    out.set(row.timeline_id, {
      row,
      session,
      operation,
      team: action.team,
      name,
      color: action.color,
      objective: canonicalText(action.objective),
      configuredModel: action.model,
      runtime: runtimeProfile(memberRows, action.model),
    });
  }
  return out;
}

function participantKey(value: string): string {
  return value.trim().replace(/^@/, "").toLocaleLowerCase();
}

/** Resolve message senders back to roster facts. Names are native addresses;
 * no timestamp or prose matching participates in this join. */
export function teamParticipants(
  members: ReadonlyMap<string, TeamMemberPresentation>,
): ReadonlyMap<string, TeamParticipantPresentation> {
  const out = new Map<string, TeamParticipantPresentation>();
  const bareTeam = new Map<string, string>();
  const ambiguous = new Set<string>();
  for (const member of members.values()) {
    const presentation = {
      color: member.color,
      runtime: member.runtime,
      sessionId: member.session?.id ?? null,
    };
    const names = [
      member.name,
      member.session?.title ?? null,
      member.session?.native_session_id ?? null,
    ];
    for (const name of names) {
      if (name === null || name.trim() === "") continue;
      const key = participantKey(name);
      out.set(`${participantKey(member.team)}\u0000${key}`, presentation);
      const previousTeam = bareTeam.get(key);
      if (previousTeam === undefined && !ambiguous.has(key)) {
        bareTeam.set(key, member.team);
        out.set(key, presentation);
      } else if (previousTeam === member.team) {
        out.set(key, presentation);
      } else {
        bareTeam.delete(key);
        ambiguous.add(key);
        out.delete(key);
      }
    }
  }
  return out;
}

/** Add only delivered boundary events from team-member transcripts to the
 * parent's chronology. The member's initial coordinator assignment stays
 * private; later coordinator follow-ups are addressable team conversation and
 * do interleave. A delivery id is the only cross-session dedupe key we trust;
 * similar prose is never silently collapsed. Parent row order remains
 * canonical, while child events enter the gap selected by their own stamp. */
export function interleaveTeamCollaboration(
  parentRows: readonly TimelineEntry[],
  rowsBySession: ReadonlyMap<string, readonly TimelineEntry[]>,
): readonly TimelineEntry[] {
  const seenTimelineIds = new Set(parentRows.map((row) => row.timeline_id));
  const seenDeliveries = new Set<string>();
  for (const row of parentRows) {
    if (row.body.type !== "collaboration") continue;
    if (row.body.event.delivery_id !== null) seenDeliveries.add(row.body.event.delivery_id);
  }

  const candidates: TimelineEntry[] = [];
  for (const rows of rowsBySession.values()) {
    for (const row of rows) {
      if (row.body.type !== "collaboration") continue;
      if (
        row.body.event.channel === "coordinator" &&
        row.body.event.kind === "assignment"
      ) {
        continue;
      }
      if (seenTimelineIds.has(row.timeline_id)) continue;
      seenTimelineIds.add(row.timeline_id);
      candidates.push(row);
    }
  }
  candidates.sort(
    (left, right) =>
      (left.at ?? Number.MAX_SAFE_INTEGER) - (right.at ?? Number.MAX_SAFE_INTEGER) ||
      left.session_id.localeCompare(right.session_id) ||
      left.ordinal - right.ordinal ||
      left.timeline_id.localeCompare(right.timeline_id),
  );
  const additions: TimelineEntry[] = [];
  for (const row of candidates) {
    const delivery = row.body.type === "collaboration" ? row.body.event.delivery_id : null;
    if (delivery !== null && seenDeliveries.has(delivery)) continue;
    if (delivery !== null) seenDeliveries.add(delivery);
    additions.push(row);
  }
  if (additions.length === 0) return parentRows;

  const buckets = Array.from({ length: parentRows.length + 1 }, () => [] as TimelineEntry[]);
  for (const row of additions) {
    const at = row.at;
    const before =
      at === null
        ? -1
        : parentRows.findIndex((parent) => parent.at !== null && parent.at > at);
    buckets[before === -1 ? parentRows.length : before].push(row);
  }

  const merged: TimelineEntry[] = [];
  for (let at = 0; at < parentRows.length; at += 1) {
    merged.push(...buckets[at], parentRows[at]);
  }
  merged.push(...buckets[parentRows.length]);
  return merged;
}

export function participantPresentation(
  participants: ReadonlyMap<string, TeamParticipantPresentation>,
  sender: string | null,
  team: string | null = null,
): TeamParticipantPresentation | null {
  if (sender === null) return null;
  const key = participantKey(sender);
  return (
    (team === null ? undefined : participants.get(`${participantKey(team)}\u0000${key}`)) ??
    participants.get(key) ??
    null
  );
}

/** Give an ordinary Agent worker exactly one subthread. The earliest proven
 * invocation owns it; subsequent delegate/resume rows are links back to that
 * same disclosure. Team children are excluded because their delivered speech
 * belongs to the shared parent chronology instead. */
export function workerThreadLinks(
  lines: readonly LedgerLine[],
  parentSessionId: string,
  sessions: ReadonlyMap<string, SessionRow>,
  excludedSessionIds: ReadonlySet<string>,
): WorkerThreadLinks {
  const anchorRowByChild = new Map<string, string>();
  const childrenByRow = new Map<string, SessionRow[]>();
  const referencedChildIdsByRow = new Map<string, string[]>();

  for (const line of lines) {
    const rowId = line.rows[0]?.timeline_id;
    if (rowId === undefined) continue;
    const childIds = new Set(line.rows.flatMap((row) => linkedChildSessionIds(row)));
    for (const childId of childIds) {
      const child = sessions.get(childId);
      if (
        child === undefined ||
        child.parent_session_id !== parentSessionId ||
        excludedSessionIds.has(child.id)
      ) {
        continue;
      }
      const anchor = anchorRowByChild.get(child.id);
      if (anchor === undefined) {
        anchorRowByChild.set(child.id, rowId);
        const anchored = childrenByRow.get(rowId);
        if (anchored === undefined) childrenByRow.set(rowId, [child]);
        else anchored.push(child);
      } else if (anchor !== rowId) {
        const references = referencedChildIdsByRow.get(rowId);
        if (references === undefined) referencedChildIdsByRow.set(rowId, [child.id]);
        else if (!references.includes(child.id)) references.push(child.id);
      }
    }
  }

  return { anchorRowByChild, childrenByRow, referencedChildIdsByRow };
}

/** Recover a teammate report's nested work only from matching native task
 * identifiers in the same member transcript. The closing report is rendered
 * above the disclosure, so the returned slice stops immediately before it. */
export function taskBoundedReportWork(
  reportRow: TimelineEntry,
  sourceRows: readonly TimelineEntry[],
): TeamReportWork | null {
  if (
    reportRow.body.type !== "collaboration" ||
    reportRow.body.event.kind !== "report" ||
    reportRow.body.event.task_id === null
  ) {
    return null;
  }
  const ordered = [...sourceRows].sort(
    (left, right) => left.ordinal - right.ordinal || left.timeline_id.localeCompare(right.timeline_id),
  );
  const reportAt = ordered.findIndex((row) => row.timeline_id === reportRow.timeline_id);
  const reportStamp = reportRow.at;
  const candidates =
    reportAt >= 0
      ? ordered.slice(0, reportAt)
      : reportStamp === null
        ? []
        : ordered.filter((row) => row.at !== null && row.at <= reportStamp);
  if (candidates.length === 0) return null;
  const taskId = reportRow.body.event.task_id;
  let assignmentAt = -1;
  for (let at = candidates.length - 1; at >= 0; at -= 1) {
    const row = candidates[at];
    if (
      row.body.type === "collaboration" &&
      row.body.event.kind === "assignment" &&
      row.body.event.task_id === taskId
    ) {
      assignmentAt = at;
      break;
    }
  }
  if (assignmentAt === -1) return null;
  const rows = candidates.slice(assignmentAt);
  return {
    rows,
    actionCount: rows.filter((row) => row.body.type === "tool_use").length,
    startedAt: rows.find((row) => row.at !== null)?.at ?? null,
    endedAt: reportRow.at,
  };
}
