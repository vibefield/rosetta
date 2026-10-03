// §05 — the room's activity act: fold or open every run at once.
//
// The preference is per ROOM, not per run: the reader who wants the build log
// gets it in one press and keeps it for the session that follows. It is stored
// the way the project tints are — the room's storage, honestly priced: it is a
// property of this machine's reader, so it does not travel until the store
// does.
//
// The act sets the room's DEFAULT; every head follows it until pressed. It
// is ONE store per room, not one state per caller, so each run's head reads it, draws
// itself open or closed accordingly, and still folds or opens on its own
// press (a press is an override; the next press of the act clears every
// override). `data-fold` on the column remains for styling only.
//
// v2: the meaning changed with transcript-fold (2026-09-09). Before, "open"
// unfolded the few runs past eight lines; now it opens every run in the
// room, so the preference starts over, folded.
import { useSyncExternalStore } from "react";
import type { KeyValueStore } from "../../lib/room-storage";
import { useRoom } from "../../room-context";

/** Under the room's storage prefix. */
const STORE_KEY = "transcript.fold.v2";

export interface FoldState {
  readonly folded: boolean;
  readonly revision: number;
}

/** One room's fold: its default, and the act that flips it. */
export interface FoldPreference {
  subscribe(listener: () => void): () => void;
  snapshot(): FoldState;
  toggle(): void;
}

const SERVER_STATE: FoldState = { folded: true, revision: 0 };

export function createFoldPreference(store: KeyValueStore | null): FoldPreference {
  /** Folded is the default: the ledger's closed sentence is the point. */
  let folded = (() => {
    try {
      return store?.getItem(STORE_KEY) !== "off";
    } catch {
      return true;
    }
  })();
  let revision = 0;
  let state: FoldState = { folded, revision };
  const listeners = new Set<() => void>();
  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    snapshot: () => state,
    toggle() {
      folded = !folded;
      revision += 1;
      state = { folded, revision };
      try {
        store?.setItem(STORE_KEY, folded ? "on" : "off");
      } catch {
        // storage refused; the preference holds for this run
      }
      for (const listener of listeners) listener();
    },
  };
}

export function useFoldPreference(): { folded: boolean; revision: number; toggle: () => void } {
  const { fold } = useRoom();
  const value = useSyncExternalStore(fold.subscribe, fold.snapshot, () => SERVER_STATE);
  return { ...value, toggle: fold.toggle };
}
