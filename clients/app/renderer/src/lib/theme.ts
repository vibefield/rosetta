import { useSyncExternalStore } from "react";

type Theme = "light" | "dark";
const STORE_KEY = "ah.theme";
const scheme = window.matchMedia("(prefers-color-scheme: dark)");

function loadPreference(): Theme | null {
  try {
    const value = window.localStorage.getItem(STORE_KEY);
    return value === "light" || value === "dark" ? value : null;
  } catch {
    return null;
  }
}

let preference = loadPreference();
let theme: Theme = preference ?? (scheme.matches ? "dark" : "light");
const listeners = new Set<() => void>();

function applyTheme(): void {
  theme = preference ?? (scheme.matches ? "dark" : "light");
  // Shared by the room, portalled viewers and mille's embedded file tree.
  document.documentElement.classList.toggle("dark", theme === "dark");
  document.documentElement.dataset.theme = theme;
  for (const listener of listeners) listener();
}

// Apply the saved palette before React's first paint. System changes remain
// live until the reader makes an explicit choice with the title-bar button.
applyTheme();
scheme.addEventListener("change", () => {
  if (preference === null) applyTheme();
});
window.addEventListener("storage", (event) => {
  if (event.key !== STORE_KEY && event.key !== null) return;
  preference = loadPreference();
  applyTheme();
});

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

function snapshot(): Theme {
  return theme;
}

function toggleTheme(): void {
  preference = theme === "dark" ? "light" : "dark";
  try {
    window.localStorage.setItem(STORE_KEY, preference);
  } catch {
    // The selection still holds for this launch when storage is unavailable.
  }
  applyTheme();
}

export function useTheme() {
  return { theme: useSyncExternalStore(subscribe, snapshot), toggleTheme };
}
