// §05 — the room's activity act: fold or open every run at once.
//
// The preference is per ROOM, not per run: the reader who wants the build log
// gets it in one press and keeps it for the session that follows. It is stored
// the way the project tints are — localStorage, honestly priced: it is a
// property of this machine's reader, so it does not travel until the store
// does.
//
// The act sets the room's DEFAULT; every head follows it until pressed. It
// is ONE store, not one state per caller, so each run's head reads it, draws
// itself open or closed accordingly, and still folds or opens on its own
// press (a press is an override; the next press of the act clears every
// override). `data-fold` on the column remains for styling only.
//
// v2: the meaning changed with transcript-fold (2026-09-09). Before, "open"
// unfolded the few runs past eight lines; now it opens every run in the
// room, so the preference starts over, folded.
import { useCallback, useSyncExternalStore } from "react";

const STORE_KEY = "ah.transcript.fold.v2";

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
