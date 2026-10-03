// The room's few remembered preferences — recent searches, dealt project
// tints, the site-icon setting, the fold — kept in the host's store under the
// host's prefix. Nothing the room writes lands outside that prefix, and a
// store that refuses (a private window, a blocked origin) costs the memory,
// never the room.

/** The two calls the room makes of a store; `localStorage` is one. */
export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** A host's store as the room uses it: every name under one prefix. */
export interface RoomStorage extends KeyValueStore {
  readonly prefix: string;
}

export const DEFAULT_STORAGE_PREFIX = "ah.";

/** The browser's localStorage, or null where it cannot be reached. */
export function browserStorage(): KeyValueStore | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function createRoomStorage(prefix: string, backend: KeyValueStore | null): RoomStorage {
  return {
    prefix,
    getItem(key) {
      if (backend === null) return null;
      try {
        return backend.getItem(`${prefix}${key}`);
      } catch {
        return null;
      }
    },
    setItem(key, value) {
      if (backend === null) return;
      try {
        backend.setItem(`${prefix}${key}`, value);
      } catch {
        // refused: the preference holds for this run
      }
    },
  };
}
