import type { TimelineEntry } from "@rosetta/protocol";
import type { ReadingSitting } from "./activity-plan";
import { activityRowKey } from "./activity-plan";

type Identity = { readonly key: string; readonly rows: readonly TimelineEntry[] };

/** Prefer the largest surviving overlap, with one owner per old key. Inserting
 * older rows must not remount an existing sitting, fold, or attachment rail.
 * Real splits/merges keep the best surviving owner; row disclosure state is
 * independently keyed by canonical identity. No historical rows are retained. */
function retainedKeys(previous: readonly Identity[], next: readonly Identity[]): string[] {
  const owner = new Map<string, string>();
  for (const group of previous) for (const row of group.rows) owner.set(activityRowKey(row), group.key);
  const matches = next.flatMap((group, index) => {
    const counts = new Map<string, number>();
    for (const row of group.rows) {
      const key = owner.get(activityRowKey(row));
      if (key !== undefined) counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    return [...counts].map(([key, count]) => ({ index, key, count }));
  }).sort((a, b) => b.count - a.count || a.index - b.index);
  const assigned = new Map<number, string>();
  const used = new Set<string>();
  for (const match of matches) {
    if (assigned.has(match.index) || used.has(match.key)) continue;
    assigned.set(match.index, match.key);
    used.add(match.key);
  }
  return next.map((group, index) => {
    const saved = assigned.get(index);
    if (saved !== undefined) return saved;
    let key = group.key;
    while (used.has(key)) key += ":split";
    used.add(key);
    return key;
  });
}

export function retainReadingIdentity(previous: readonly ReadingSitting[], next: readonly ReadingSitting[]): ReadingSitting[] {
  const sittingKeys = retainedKeys(previous.map(x => ({ ...x.sitting, key: x.renderKey ?? x.sitting.key })), next.map(x => x.sitting));
  return next.map(({ sitting, blocks }, index) => {
    const old = previous.find(x => (x.renderKey ?? x.sitting.key) === sittingKeys[index]);
    const groups = blocks.filter(block => block.kind === "activity");
    const keys = retainedKeys(old?.blocks.filter(block => block.kind === "activity") ?? [], groups);
    let groupIndex = 0;
    return {
      sitting,
      renderKey: sittingKeys[index]!,
      blocks: blocks.map(block => block.kind === "activity" ? { ...block, key: keys[groupIndex++]! } : block),
    };
  });
}
