// Delegated-work presentation facts.
//
// The canonical timeline deliberately stores evidence, not UI geometry. This
// module is the small pure seam that turns proven Agent Team relations into a
// shared chronology and roster without teaching React about Claude envelopes.
// Ordinary Agent workers never enter these helpers: they remain invocation-
// rooted child documents.
import type {
  ParticipantColor,
  ResponseRuntime,
  SessionRow,
  TimelineEntry,
} from "@rosetta/protocol";
import type { LedgerLine } from "./ledger.ts";
import { linkedChildSessionIds } from "./ledger.ts";
import { canonicalText } from "./vocabulary.ts";
import { withinHistoryWindow } from "./history-window.ts";

export type RuntimeProfileBasis = "observed" | "varies" | "configured" | "unknown";

export interface RuntimeProfile {
  readonly basis: RuntimeProfileBasis;
  readonly label: string;
  readonly model: string | null;
  readonly effort: string | null;
  readonly profileCount: number;
}

export type TeamMemberOperation = "start" | "known" | "add" | "resume";

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

/** Workflow workers belong to the workflow hierarchy even when pagination
 * leaves their owning workflow row outside the loaded parent page. The title
 * fallback keeps projections made before `workflow_run_id` was retained from
 * leaking hundreds of legacy workflow workers into the parent timeline. */
export function isWorkflowOwnedSession(session: SessionRow): boolean {
  if ((session.delegation?.workflow_run_id?.trim().length ?? 0) > 0) return true;
  return session.delegation !== null
    && session.title?.trim().toLowerCase() === "workflow-subagent";
}

/** Only genuinely standalone workers may use the timestamp-based fallback.
 * Workflow workers wait for their owning run and mount under their exact
 * phase/worker row through `workerThreadLinks`. */
export function standaloneWorkerSessions(
  children: readonly SessionRow[],
  links: WorkerThreadLinks,
  indexedWorkflowChildren: ReadonlySet<string> = new Set(),
): readonly SessionRow[] {
  return children.filter(
    (child) => !links.anchorRowByChild.has(child.id)
      && !indexedWorkflowChildren.has(child.id)
      && !isWorkflowOwnedSession(child),
  );
}

export interface TeamReportWork {
  readonly rows: readonly TimelineEntry[];
  readonly actionCount: number;
  readonly startedAt: number | null;
  readonly endedAt: number | null;
}

export interface TeamChronology {
  readonly rows: readonly TimelineEntry[];
  /** Every suppressed transport copy points at the one rendered delivery. */
  readonly duplicateRepresentatives: ReadonlyMap<string, string>;
}

export type CollaborationTimeBasis = "authored" | "observed_arrival" | "unknown";

export interface CollaborationStamp {
  readonly at: number | null;
  readonly basis: CollaborationTimeBasis;
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
  responseRuntimes: readonly ResponseRuntime[] = [],
): RuntimeProfile {
  const responses = new Map<string, { model: string | null; effort: string | null }>();
  for (const runtime of responseRuntimes) {
    const current = responses.get(runtime.response_id) ?? { model: null, effort: null };
    responses.set(runtime.response_id, {
      model: current.model ?? runtime.model,
      effort: current.effort ?? runtime.effort,
    });
  }
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
        (child) =>
          child.parent_session_id === parentSessionId &&
          (child.role === "team" || child.delegation?.team != null),
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
  responseRuntimesBySession: ReadonlyMap<string, readonly ResponseRuntime[]> = new Map(),
  historyComplete = true,
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
      action.resume_id !== null
        ? "resume"
        : seenTeams.has(action.team)
          ? "add"
          : historyComplete
            ? "start"
            : "known";
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
      color: action.color ?? session?.delegation?.color ?? null,
      objective: canonicalText(action.objective),
      configuredModel: action.model ?? session?.delegation?.configured_model ?? null,
      runtime: runtimeProfile(
        memberRows,
        action.model ?? session?.delegation?.configured_model ?? null,
        action.child_session_id === null
          ? []
          : (responseRuntimesBySession.get(action.child_session_id) ?? []),
      ),
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
  knownSessions: readonly SessionRow[] = [],
  rowsBySession: ReadonlyMap<string, readonly TimelineEntry[]> = new Map(),
  responseRuntimesBySession: ReadonlyMap<string, readonly ResponseRuntime[]> = new Map(),
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
  for (const session of knownSessions) {
    const team = session.delegation?.team ?? null;
    const name = session.title ?? session.native_session_id;
    const presentation = {
      color: session.delegation?.color ?? null,
      runtime: runtimeProfile(
        rowsBySession.get(session.id) ?? [],
        session.delegation?.configured_model ?? null,
        responseRuntimesBySession.get(session.id) ?? [],
      ),
      sessionId: session.id,
    };
    const key = participantKey(name);
    if (team !== null) out.set(`${participantKey(team)}\u0000${key}`, presentation);
    if (!ambiguous.has(key)) {
      const previousTeam = bareTeam.get(key);
      const teamKey = team ?? "";
      if (previousTeam === undefined || previousTeam === teamKey) {
        bareTeam.set(key, teamKey);
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

/** Native authored time is the shared-chronology stamp when present. The
 * containing row time is an explicitly labelled observed-delivery fallback. */
export function collaborationStamp(row: TimelineEntry): CollaborationStamp {
  if (row.body.type !== "collaboration") {
    return { at: row.at, basis: row.at === null ? "unknown" : "observed_arrival" };
  }
  if (row.body.event.authored_at !== null) {
    return { at: row.body.event.authored_at, basis: "authored" };
  }
  return {
    at: row.at,
    basis: row.at === null ? "unknown" : "observed_arrival",
  };
}

/** Add only delivered boundary events from team-member transcripts to the
 * parent's chronology. The member's initial coordinator assignment stays
 * private; later coordinator follow-ups are addressable team conversation and
 * do interleave. A delivery id is the only cross-session dedupe key we trust;
 * similar prose is never silently collapsed. Parent row order remains
 * canonical, while child events enter the gap selected by native authored
 * time, or their explicitly labelled observed-arrival fallback. */
export function teamChronology(
  parentRows: readonly TimelineEntry[],
  rowsBySession: ReadonlyMap<string, readonly TimelineEntry[]>,
  coverage?: { readonly hasEarlier: boolean; readonly ready: boolean },
): TeamChronology {
  const seenTimelineIds = new Set(parentRows.map((row) => row.timeline_id));
  const duplicateRepresentatives = new Map<string, string>();

  const candidates: TimelineEntry[] = [];
  for (const rows of rowsBySession.values()) {
    for (const row of rows) {
      if (row.body.type !== "collaboration") continue;
      if (coverage !== undefined && (!coverage.ready ||
        !withinHistoryWindow(collaborationStamp(row).at, parentRows, coverage.hasEarlier))) continue;
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
      (collaborationStamp(left).at ?? Number.MAX_SAFE_INTEGER) -
        (collaborationStamp(right).at ?? Number.MAX_SAFE_INTEGER) ||
      left.session_id.localeCompare(right.session_id) ||
      left.ordinal - right.ordinal ||
      left.timeline_id.localeCompare(right.timeline_id),
  );

  // Delivered boundary evidence is the representative for a matching
  // sender-side tool receipt. Parent boundary rows win over child copies;
  // otherwise the earliest canonical child candidate wins. Text never participates.
  const representativeByDelivery = new Map<string, TimelineEntry>();
  for (const row of [...parentRows, ...candidates]) {
    if (row.body.type !== "collaboration") continue;
    const delivery = row.body.event.delivery_id;
    if (delivery !== null && !representativeByDelivery.has(delivery)) {
      representativeByDelivery.set(delivery, row);
    }
  }

  const retainedParent: TimelineEntry[] = [];
  for (const row of parentRows) {
    const deliveryIds =
      row.body.type === "collaboration"
        ? [row.body.event.delivery_id]
        : row.body.type === "tool_use" && row.body.tool.action.type === "collaborate"
          ? [
              row.body.tool.action.delivery_id,
              row.body.tool.correlation.key,
              row.body.tool.native.id,
            ]
          : [];
    const representative = deliveryIds
      .filter((value): value is string => value !== null)
      .map((delivery) => representativeByDelivery.get(delivery))
      .find((candidate) => candidate !== undefined);
    if (representative !== undefined && representative.timeline_id !== row.timeline_id) {
      duplicateRepresentatives.set(row.timeline_id, representative.timeline_id);
      continue;
    }
    retainedParent.push(row);
  }

  const additions: TimelineEntry[] = [];
  for (const row of candidates) {
    const delivery = row.body.type === "collaboration" ? row.body.event.delivery_id : null;
    const representative = delivery === null ? undefined : representativeByDelivery.get(delivery);
    if (representative !== undefined && representative.timeline_id !== row.timeline_id) {
      duplicateRepresentatives.set(row.timeline_id, representative.timeline_id);
      continue;
    }
    additions.push(row);
  }
  if (additions.length === 0) {
    return {
      rows: retainedParent.length === parentRows.length ? parentRows : retainedParent,
      duplicateRepresentatives,
    };
  }

  const buckets = Array.from(
    { length: retainedParent.length + 1 },
    () => [] as TimelineEntry[],
  );
  for (const row of additions) {
    const at = collaborationStamp(row).at;
    const before =
      at === null
        ? -1
        : retainedParent.findIndex((parent) => parent.at !== null && parent.at > at);
    buckets[before === -1 ? retainedParent.length : before].push(row);
  }

  const merged: TimelineEntry[] = [];
  for (let at = 0; at < retainedParent.length; at += 1) {
    merged.push(...buckets[at], retainedParent[at]);
  }
  merged.push(...buckets[retainedParent.length]);
  return { rows: merged, duplicateRepresentatives };
}

export function interleaveTeamCollaboration(
  parentRows: readonly TimelineEntry[],
  rowsBySession: ReadonlyMap<string, readonly TimelineEntry[]>,
): readonly TimelineEntry[] {
  return teamChronology(parentRows, rowsBySession).rows;
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
    const nativeToolIds = new Set(
      line.rows.flatMap((row) =>
        row.body.type === "tool_use"
          ? [row.body.tool.correlation.key, row.body.tool.native.id].filter(
              (value): value is string => value !== null,
            )
          : [],
      ),
    );
    for (const child of sessions.values()) {
      if (
        child.parent_session_id === parentSessionId &&
        !excludedSessionIds.has(child.id) &&
        child.delegation?.parent_tool_native_id != null &&
        nativeToolIds.has(child.delegation.parent_tool_native_id)
      ) {
        childIds.add(child.id);
      }
    }
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

/** Recover report work only when canonical evidence names the exact source
 * session and inclusive timeline bounds. A shared task id alone is not a
 * safe range: teammates can overlap work on the same task. */
export function taskBoundedReportWork(
  reportRow: TimelineEntry,
  sourceRows: readonly TimelineEntry[],
): TeamReportWork | null {
  if (
    reportRow.body.type !== "collaboration" ||
    reportRow.body.event.kind !== "report" ||
    reportRow.body.event.source_session_id === null ||
    reportRow.body.event.source_response_id === null ||
    reportRow.body.event.work_start_timeline_id === null ||
    reportRow.body.event.work_end_timeline_id === null
  ) {
    return null;
  }
  const event = reportRow.body.event;
  const ordered = [...sourceRows].sort(
    (left, right) => left.ordinal - right.ordinal || left.timeline_id.localeCompare(right.timeline_id),
  );
  if (
    ordered.length === 0 ||
    ordered.some((row) => row.session_id !== event.source_session_id)
  ) {
    return null;
  }
  const startAt = ordered.findIndex(
    (row) => row.timeline_id === event.work_start_timeline_id,
  );
  const endAt = ordered.findIndex(
    (row) => row.timeline_id === event.work_end_timeline_id,
  );
  if (startAt === -1 || endAt < startAt) return null;
  const rows = ordered.slice(startAt, endAt + 1);
  if (!rows.some((row) => row.response?.id === event.source_response_id)) return null;
  return {
    rows,
    actionCount: rows.filter((row) => row.body.type === "tool_use").length,
    startedAt: rows.find((row) => row.at !== null)?.at ?? null,
    endedAt: [...rows].reverse().find((row) => row.at !== null)?.at ?? null,
  };
}
