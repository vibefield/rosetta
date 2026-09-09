// §05 — the room's activity act: fold or open every run at once.
//
// The preference is per ROOM, not per run: the reader who wants the build log
// gets it in one press and keeps it for the session that follows. It is stored
// the way the project tints are — localStorage, honestly priced: it is a
// property of this machine's reader, so it does not travel until the store
// does.
//
// The switch itself is CSS. `data-fold="off"` on the column reveals every
// ladder and keeps every head as its ladder's title (R-4), so flipping the
// room never walks React state and a run the reader opened by hand keeps its
// own `aria-expanded`. It is ONE store, not one state per caller, so a head
// can read it too: while every run is open the head is not a control.
import { useCallback, useSyncExternalStore } from "react";

const STORE_KEY = "ah.transcript.fold.v1";

/** Folded is the default: the ledger's closed sentence is the point. */
function load(): boolean {
  try {
    return localStorage.getItem(STORE_KEY) !== "off";
  } catch {
    return true;
  }
}

let folded = load();
const listeners = new Set<() => void>();

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function snapshot(): boolean {
  return folded;
}

export function useFoldPreference(): { folded: boolean; toggle: () => void } {
  const value = useSyncExternalStore(subscribe, snapshot, () => true);
  const toggle = useCallback(() => {
    folded = !folded;
    try {
      localStorage.setItem(STORE_KEY, folded ? "on" : "off");
    } catch {
      // storage refused; the preference holds for this run
    }
    for (const listener of listeners) listener();
  }, []);
  return { folded: value, toggle };
}
