/** `⌘K` on a Mac, `Ctrl K` elsewhere — the label follows the modifier the
 * handler actually listens for. */
export function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return true;
  return /Mac|iPhone|iPad/.test(navigator.platform) || /Mac/.test(navigator.userAgent);
}

export function shortcutLabel(key: string, shift = false): string {
  const mac = isMacPlatform();
  const parts = [mac ? "⌘" : "Ctrl", shift ? (mac ? "⇧" : "Shift") : null, key].filter(
    (part): part is string => part !== null,
  );
  return mac ? parts.join("") : parts.join(" ");
}

/** The primary modifier of a keyboard event on this platform. */
export function hasPrimaryModifier(event: KeyboardEvent | { metaKey: boolean; ctrlKey: boolean }): boolean {
  return isMacPlatform() ? event.metaKey : event.ctrlKey;
}

/** What a host says about the key that opens its search sheet (RO-14). The
 * room binds no key of its own — the host's window does (rosetta's app binds
 * ⌘K) — so the room prints only what the host says it binds: undefined, the
 * room's own label for ⌘K (`shortcutLabel("K")`); a label, that label; null,
 * no key at all. */
export type SearchShortcut = { readonly label: string } | null | undefined;

/** The legend the launcher prints, or null for none: no search, or a host
 * that binds no key. */
export function searchKeyLegend(search: { readonly shortcut?: SearchShortcut } | null): string | null {
  if (search === null) return null;
  if (search.shortcut === undefined) return shortcutLabel("K");
  return search.shortcut === null ? null : search.shortcut.label;
}

/** The empty transcript's own sentence: it names a key only when the
 * launcher prints one, so it never names a key the host does not bind. */
export function emptyTranscriptPrompt(legend: string | null): string {
  return legend === null
    ? "Pick a session."
    : `Pick a session — or press ${legend} to search, which is the front door.`;
}
