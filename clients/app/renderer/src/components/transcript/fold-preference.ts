// §05 — the room's activity act: fold or open every run at once.
//
// The preference is per ROOM, not per run: the reader who wants the build log
// gets it in one press and keeps it for the session that follows. It is stored
// the way the project tints are — localStorage, honestly priced: it is a
// property of this machine's reader, so it does not travel until the store
// does.
//
// The switch itself is CSS. `data-fold="off"` on the column hides every fold
// line and reveals every ladder, so flipping the room never walks React state
// and a run the reader opened by hand keeps its own `aria-expanded`.
import { useCallback, useState } from "react";

const STORE_KEY = "ah.transcript.fold.v1";

/** Folded is the default: the ledger's closed sentence is the point. */
function load(): boolean {
  try {
    return localStorage.getItem(STORE_KEY) !== "off";
  } catch {
    return true;
  }
}

export function useFoldPreference(): { folded: boolean; toggle: () => void } {
  const [folded, setFolded] = useState(load);
  const toggle = useCallback(() => {
    setFolded((value) => {
      const next = !value;
      try {
        localStorage.setItem(STORE_KEY, next ? "on" : "off");
      } catch {
        // storage refused; the preference holds for this run
      }
      return next;
    });
  }, []);
  return { folded, toggle };
}
