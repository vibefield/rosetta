// A host's act (`RoomAct`), drawn as one of the room's own buttons in the
// slot that holds it. The room presses it, or opens its rows (`HostMenu`),
// and draws nothing about what it means: the host's glyph or word is the
// face, its label the name.
import type { RoomAct } from "../host-inputs";
import { HostMenuButton } from "./HostMenu";

/** Where an act sits, which decides its face: a session row's quiet act
 * (a 20px pill under the pointer), ②'s head (the room's 26px icon button;
 * its one primary on the go fill), or ④'s head (a pill, the stepper's). */
export type HostActPlace = "row" | "head" | "pill";

export function HostAct({ act, place }: { readonly act: RoomAct; readonly place: HostActPlace }) {
  const className = place === "head" ? "ah-iconbtn ah-hostact" : "ah-hostact";
  const primary = place === "head" && act.primary === true ? "" : undefined;
  // Its rows are drawn over the columns (`HostMenu`), so a row's act may open
  // them as well as a head's.
  if (act.menu !== undefined) {
    return (
      <HostMenuButton
        menu={act.menu}
        className={className}
        label={act.label}
        title={act.label}
        disabled={act.disabled}
        align="end"
        data={{ "data-place": place, "data-act": act.key, "data-primary": primary }}
      >
        {act.content}
      </HostMenuButton>
    );
  }
  return (
    <button
      type="button"
      className={className}
      data-place={place}
      data-act={act.key}
      data-primary={primary}
      title={act.label}
      aria-label={act.label}
      disabled={act.disabled === true}
      onClick={(event) => {
        // The act is the host's; whatever lies under it stays as it was.
        event.stopPropagation();
        act.onPress?.();
      }}
    >
      {act.content}
    </button>
  );
}
