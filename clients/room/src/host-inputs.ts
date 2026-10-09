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

/* ── everything, as `HistoryRoomProps` carries it ───────────────────────── */

export interface RoomHostInputs {
  /** R1 — a project's colour (any CSS colour). Absent, or null for a
   * project: the room's own dealt tint. */
  readonly projectTint?: ((projectId: string) => string | null | undefined) | undefined;
}

/** Only the host's inputs, picked out of the room's props. */
export function hostInputsOf(props: RoomHostInputs): RoomHostInputs {
  return {
    projectTint: props.projectTint,
  };
}

const HostInputsContext = createContext<RoomHostInputs>({});
export const HostInputsProvider = HostInputsContext.Provider;

/** The host's inputs, as the room's parts read them; none outside a room. */
export function useHostInputs(): RoomHostInputs {
  return useContext(HostInputsContext);
}
