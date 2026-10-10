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
import type { SessionRow } from "@rosetta/protocol";

/* ── the room's popover rows, as a host fills them ──────────────────────── */

/** One row of a menu the room draws for its host. */
export interface RoomMenuItem {
  readonly key: string;
  /** What leads the row — a mark the host draws. Never interactive. */
  readonly mark?: ReactNode;
  readonly label: ReactNode;
  /** A second line under the label (a path, "asleep · 2 h"). */
  readonly detail?: ReactNode;
  /** At the row's right end ("here", a count, "last used"). */
  readonly aside?: ReactNode;
  /** The chosen one: drawn with the check. */
  readonly checked?: boolean | undefined;
  /** Drawn, not choosable; its `detail` says why. */
  readonly disabled?: boolean | undefined;
  /** One step in, under the row above it (a worktree under its checkout). */
  readonly depth?: 0 | 1 | undefined;
  /** The words a menu's filter matches. Absent: the row always shows. */
  readonly filterText?: string | undefined;
  readonly onSelect?: (() => void) | undefined;
}

export interface RoomMenuGroup {
  readonly key: string;
  /** The group's head ("Starts in", a desktop's mark and name). */
  readonly title?: ReactNode;
  readonly items: readonly RoomMenuItem[];
}

/** A menu: groups of rows. The room opens it under the control that owns it,
 * closes it on a choice, a press outside or Escape, and moves through its
 * rows with the arrow keys while it is open. */
export interface RoomMenu {
  /** Its accessible name. */
  readonly label: string;
  readonly groups: readonly RoomMenuGroup[];
  /** Past this many rows a filter heads the menu; its placeholder is
   * `filterPlaceholder` ("Filter 34 folders"). Absent: never. */
  readonly filterAfter?: number | undefined;
  readonly filterPlaceholder?: string | undefined;
  /** What the menu says with no rows to show. */
  readonly empty?: ReactNode;
}

/** A host act, drawn as one of the room's own buttons in the slot that holds
 * it. The room presses it and draws nothing about what it means. */
export interface RoomAct {
  readonly key: string;
  /** Its accessible name and its tooltip. */
  readonly label: string;
  /** What the button shows: the host's glyph, its word, or both. */
  readonly content: ReactNode;
  /** The slot's one primary act, on the room's primary fill (② head only). */
  readonly primary?: boolean | undefined;
  readonly disabled?: boolean | undefined;
  readonly onPress?: (() => void) | undefined;
  /** The act opens these rows instead of pressing (② and ④ heads only; a
   * session row's act is pressed, never a menu). */
  readonly menu?: RoomMenu | undefined;
}

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

/* ── R4: a session row ② ───────────────────────────────────────────────── */

/** A session row's host decorations. */
export interface RoomSessionRow {
  /** The host's own key for the session. The room never reads it: it rides
   * the selection (`RoomSelection.hostKey`) back out, untouched. */
  readonly hostKey?: string | undefined;
  /** Drawn in the row's id slot in place of the native id (the host's mark
   * and the folder's path). Never interactive. */
  readonly id?: ReactNode;
  /** The id slot's tooltip. Absent with `id`: none. */
  readonly idTitle?: string | undefined;
  /** At the row's right end, before its time (Resume, Jump to). Pressed,
   * never a menu; the row itself stays the selection. */
  readonly act?: RoomAct | undefined;
  /** Where the act would be, when there is none: the reason, quiet. */
  readonly reason?: ReactNode;
}

/* ── R5: the heads ② ④ ─────────────────────────────────────────────────── */

/** What ② knows when it asks for its head's acts. */
export interface RoomProjectContext {
  /** The folder ③ shows, by the host's key (R6), or null. */
  readonly folder: string | null;
}

/** ④'s head: the host's acts, before the room's activity act, or the reason
 * after the range. */
export interface RoomHeadActs {
  readonly acts?: readonly RoomAct[] | undefined;
  readonly reason?: ReactNode;
}

/* ── R6: ③'s folder ─────────────────────────────────────────────────────── */

/** One folder the host offers ③. */
export interface RoomFolder {
  /** The host's key — never a path: two machines spell paths alike. */
  readonly key: string;
  /** The folder: what the head names and what the worktree opens. */
  readonly path: string;
  /** In front of the path, in the head and in its row. */
  readonly mark?: ReactNode;
  /** Its row's words. Absent: the path, home-abbreviated. */
  readonly label?: ReactNode;
  /** Its row's second line (the branch, who works there). */
  readonly detail?: ReactNode;
  readonly aside?: ReactNode;
  /** One step in, under the folder above it (a linked worktree). */
  readonly depth?: 0 | 1 | undefined;
  /** The head's caption while it is shown ("Studio · main"). Absent: git's. */
  readonly caption?: string | undefined;
  /** This folder cannot be opened here: ③ draws this, the host's honest
   * state, instead of opening it. */
  readonly elsewhere?: ReactNode;
}

export interface RoomFolderGroup {
  readonly key: string;
  readonly title?: ReactNode;
  readonly folders: readonly RoomFolder[];
}

/** ③'s folder list for one project: its head becomes the dropdown. */
export interface RoomFolders {
  readonly groups: readonly RoomFolderGroup[];
  /** The folder ③ opens on while no session is chosen (the usual place).
   * Absent: the room's own root. */
  readonly initial?: string | undefined;
  /** The folder a session ran in: ③ follows it when the session is chosen.
   * Absent, or null for a session: the room's own root for its folder. */
  readonly of?: ((session: SessionRow) => string | null | undefined) | undefined;
  /** Past this many folders a filter heads the list. */
  readonly filterAfter?: number | undefined;
  readonly filterPlaceholder?: string | undefined;
  /** ③ shows this folder now — the reader's pick or the session followed;
   * null when it shows none of the host's. */
  readonly onShow?: ((key: string | null) => void) | undefined;
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
  /** R4 — a session row's decorations and host key. */
  readonly sessionRow?: ((session: SessionRow) => RoomSessionRow | null | undefined) | undefined;
  /** R5 — ②'s head acts, after the room's own; one may be `primary`. */
  readonly projectActs?:
    | ((projectId: string, context: RoomProjectContext) => readonly RoomAct[] | null | undefined)
    | undefined;
  /** R5 — a card under ②'s head, above its list (suggestions). */
  readonly projectCard?: ((projectId: string) => ReactNode) | undefined;
  /** R5 — ④'s head, for the session it names. */
  readonly sessionActs?: ((session: SessionRow) => RoomHeadActs | null | undefined) | undefined;
  /** R6 — ③'s folders, per project. */
  readonly folders?: ((projectId: string) => RoomFolders | null | undefined) | undefined;
}

/** Only the host's inputs, picked out of the room's props. */
export function hostInputsOf(props: RoomHostInputs): RoomHostInputs {
  return {
    projectTint: props.projectTint,
    projectRow: props.projectRow,
    projectSections: props.projectSections,
    sessionSections: props.sessionSections,
    sessionRow: props.sessionRow,
    projectActs: props.projectActs,
    projectCard: props.projectCard,
    sessionActs: props.sessionActs,
    folders: props.folders,
  };
}

const HostInputsContext = createContext<RoomHostInputs>({});
export const HostInputsProvider = HostInputsContext.Provider;

/** The host's inputs, as the room's parts read them; none outside a room. */
export function useHostInputs(): RoomHostInputs {
  return useContext(HostInputsContext);
}
