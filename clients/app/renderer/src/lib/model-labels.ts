export function modelColor(modelId: string): string {
  let hash = 2166136261;
  for (let index = 0; index < modelId.length; index += 1) {
    hash ^= modelId.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `var(--vf-accent-${((hash >>> 0) % 8) + 1})`;
}

export function formatModelLabel(modelId: string): string {
  return modelId.startsWith("claude-") ? modelId.slice("claude-".length) : modelId;
}

/** Resolve palette collisions once for a project's model set, so selecting
 * another day keeps a model's color and adjacent legend entries stay distinct. */
export function createModelColors(ids: readonly string[]): Readonly<Record<string, string>> {
  const colors: Record<string, string> = Object.create(null);
  const used = new Set<string>();
  for (const id of [...new Set(ids)].sort()) {
    const preferred = modelColor(id);
    let color = preferred;
    for (let index = 1; used.has(color) && index <= 8; index++) color = `var(--vf-accent-${index})`;
    if (used.has(color)) color = `color-mix(in srgb, ${preferred} 65%, var(--vf-text-primary))`;
    colors[id] = color;
    used.add(color);
  }
  return colors;
}
