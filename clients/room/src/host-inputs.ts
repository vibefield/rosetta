// What a host may hand the room beyond its ports: its own colours, marks,
// sections, acts, folders and filters, which the room DRAWS in its own
// vocabulary and never interprets. Every one is optional, and with none of
// them the room is exactly what rosetta's app shows — the reference host
// passes none. The room stays unaware of what a host means by them: a host
// key is passed through untouched, a host mark is drawn where it is put, and
// a host act is pressed, never second-guessed.
//
// Ids are the room's own: a project's `ProjectRow.id`, a session's
// `SessionRow.id`, as the host's catalog handed them in.
import { createContext, useContext } from "react";
import type { ReactNode } from "react";

/* ── R2–R3: the ledger ① and the lists ─────────────────────────────────── */

/** A project row's host decorations (R2). None of them is interactive: the
 * row is one button. */
export interface RoomProjectRow {
  /** Drawn at the start of the path line, before the path (a desktop's marks). */
  readonly marks?: ReactNode;
  /** A second line under the path (a disambiguation, "3 sessions on Studio"). */
  readonly line?: ReactNode;
  /** How many of the project's sessions are live now: above zero, the row's
   * time slot says "n live" in the room's live ink. */
  readonly live?: number | undefined;
}

/** A section of a list (R3). Given a list's sections, the room draws them in
 * the order given; a row two sections name is drawn in the first, and a row
 * no section names is drawn in the section without `ids` — or nowhere, when
 * every section names its rows (which is how a host filter hides rows). */
export interface RoomSection {
  readonly key: string;
  /** Its head, in the host's words ("Live now"). Absent: no head. */
  readonly title?: ReactNode;
  /** Its rows. Absent: every row no other section names. */
  readonly ids?: readonly string[] | undefined;
  /** Whose order the rows keep. `host` (the default when `ids` is given): as
   * `ids` lists them. `room`: the room's own — the ledger's rank switch, or a
   * session list's days, with the day heads. A section without `ids` always
   * keeps the room's. */
  readonly order?: "host" | "room" | undefined;
  /** A foot section: drawn folded, as one line of the host's words and the
   * section's count, ending in `act` ("Show older"); the line opens and
   * closes it. A folded section holding the selected row opens. */
  readonly fold?: {
    readonly label: ReactNode;
    readonly act: string;
    /** Absent: how many rows the section holds. */
    readonly count?: number | undefined;
  } | undefined;
}

/* ── everything, as `HistoryRoomProps` carries it ───────────────────────── */

export interface RoomHostInputs {
  /** R1 — a project's colour (any CSS colour). Absent, or null for a
   * project: the room's own dealt tint. */
  readonly projectTint?: ((projectId: string) => string | null | undefined) | undefined;
  /** R2 — a project row's decorations. */
  readonly projectRow?: ((projectId: string) => RoomProjectRow | null | undefined) | undefined;
  /** R3 — ①'s sections. */
  readonly projectSections?: readonly RoomSection[] | undefined;
  /** R3 — ②'s sections, per project. */
  readonly sessionSections?: ((projectId: string) => readonly RoomSection[] | null | undefined) | undefined;
}

/** Only the host's inputs, picked out of the room's props. */
export function hostInputsOf(props: RoomHostInputs): RoomHostInputs {
  return {
    projectTint: props.projectTint,
    projectRow: props.projectRow,
    projectSections: props.projectSections,
    sessionSections: props.sessionSections,
  };
}

const HostInputsContext = createContext<RoomHostInputs>({});
export const HostInputsProvider = HostInputsContext.Provider;

/** The host's inputs, as the room's parts read them; none outside a room. */
export function useHostInputs(): RoomHostInputs {
  return useContext(HostInputsContext);
}
