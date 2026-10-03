// State is keyed by canonical events, not their current paging/group wrappers.
type Override = { readonly open: boolean; readonly revision: number; readonly sequence: number };
export function createDisclosureStore() {
  const rows = new Map<string, boolean>();
  const groups = new Map<string, Override>();
  const listeners = new Set<() => void>();
  let sequence = 0;
  const changed = () => { sequence += 1; for (const listener of listeners) listener(); };
  return {
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    version: () => sequence,
    row: (key: string) => rows.get(key),
    setRow(key: string, open: boolean) { if (rows.get(key) !== open) { rows.set(key, open); changed(); } },
    group(keys: readonly string[], revision: number): boolean | undefined {
      let latest: Override | undefined;
      for (const key of keys) {
        const value = groups.get(key);
        if (value?.revision === revision && (latest === undefined || value.sequence > latest.sequence)) latest = value;
      }
      return latest?.open;
    },
    setGroup(keys: readonly string[], open: boolean, revision: number) {
      for (const key of keys) groups.set(key, { open, revision, sequence: sequence + 1 });
      changed();
    },
  };
}
