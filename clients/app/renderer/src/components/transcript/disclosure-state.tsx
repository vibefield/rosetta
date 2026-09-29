// Session-local disclosure state survives an event moving into an older page's
// activity group. It is deliberately separate from the room's fold preference.
import { createContext, useCallback, useContext, useState, useSyncExternalStore } from "react";
import type { ReactNode } from "react";

import { createDisclosureStore } from "./disclosure-store";

const DisclosureContext = createContext<ReturnType<typeof createDisclosureStore> | null>(null);

export function DisclosureState({ children }: { readonly children: ReactNode }) {
  const [store] = useState(createDisclosureStore);
  return <DisclosureContext.Provider value={store}>{children}</DisclosureContext.Provider>;
}

const subscribeNothing = () => () => {};
const noValue = () => undefined;
export function useRowDisclosure(key: string, initial = false): [boolean, (value: boolean | ((open: boolean) => boolean)) => void] {
  const store = useContext(DisclosureContext);
  const [fallback, setFallback] = useState(initial);
  const read = useCallback(() => store?.row(key), [store, key]);
  const saved = useSyncExternalStore(store?.subscribe ?? subscribeNothing, read, noValue);
  const open = saved ?? fallback;
  const setOpen = useCallback((value: boolean | ((open: boolean) => boolean)) => {
    const next = typeof value === "function" ? value(store?.row(key) ?? fallback) : value;
    if (store === null) setFallback(next);
    else store.setRow(key, next);
  }, [store, key, fallback]);
  return [open, setOpen];
}

export function useGroupDisclosure(keys: readonly string[], revision: number): [boolean | undefined, (open: boolean) => void] {
  const store = useContext(DisclosureContext);
  useSyncExternalStore(store?.subscribe ?? subscribeNothing, store?.version ?? (() => 0), () => 0);
  return [store?.group(keys, revision), (open) => store?.setGroup(keys, open, revision)];
}
