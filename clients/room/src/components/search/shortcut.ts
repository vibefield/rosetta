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
