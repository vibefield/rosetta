// A host's act (`RoomAct`), drawn as one of the room's own buttons in the
// slot that holds it. The room presses it and draws nothing about what it
// means: the host's glyph or word is the face, its label the name.
import type { RoomAct } from "../host-inputs";

/** Where an act sits, which decides its face: a session row's quiet act
 * (a 20px pill under the pointer). */
export type HostActPlace = "row";

export function HostAct({ act, place }: { readonly act: RoomAct; readonly place: HostActPlace }) {
  return (
    <button
      type="button"
      className="ah-hostact"
      data-place={place}
      data-act={act.key}
      title={act.label}
      aria-label={act.label}
      disabled={act.disabled === true}
      onClick={(event) => {
        // The act is the host's; the row under it stays the selection.
        event.stopPropagation();
        act.onPress?.();
      }}
    >
      {act.content}
    </button>
  );
}
