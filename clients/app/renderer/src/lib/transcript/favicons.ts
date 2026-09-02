// Site icons, as the renderer sees them.
//
// A search's sources are the one place in this room where identity should
// carry colour: a site's own icon is content, and it is the fastest way to
// tell GitHub from MDN from somebody's blog. The icon is not in the protocol
// — a source carries a url and a title and nothing else — so it has to be
// fetched, and fetching is a network act the renderer is not allowed to
// perform. It asks the shell (`DesktopHost.favicon`), which validates the
// host, dials the SITE rather than a favicon service, and caches per host.
//
// Because it IS a network act, it is named and it is optional: `fetch site
// icons`, on by default, off in one call. Off means off — no request is made,
// and every site wears the globe.
//
// This module holds the renderer's half: the setting, one shared per-host
// map so eight lines from one site produce one ask, and the hook the
// components read. It deliberately holds no component and no markup.

import { useCallback, useSyncExternalStore } from "react";
import { desktopHost } from "../desktop-host";

/** Versioned like `ah.tintSeq.v1`, and stored the same way: a renderer
 * preference that has no home on the daemon side yet. */
const STORE_KEY = "ah.siteIcons.v1";

/** What a site wears while nothing is known, and what it wears when the
 * answer was no. `off` is the setting; `globe` is the outcome. */
export type FaviconState = "off" | "loading" | "ready" | "globe";

export interface FaviconResult {
  /** A `data:` URL, or null in every state but `ready`. */
  readonly src: string | null;
  readonly state: FaviconState;
}

const OFF: FaviconResult = { src: null, state: "off" };
const LOADING: FaviconResult = { src: null, state: "loading" };
const GLOBE: FaviconResult = { src: null, state: "globe" };

// ── the setting ───────────────────────────────────────────────────────────

let enabled: boolean | null = null;

/** Whether this room may ask the shell for site icons. On by default: the
 * design record's answer to "should this happen at all" was yes, with a name
 * and a switch — not no. */
export function siteIconsEnabled(): boolean {
  if (enabled === null) {
    try {
      // Anything but an explicit "off" is on, so a blocked or corrupt store
      // lands on the default rather than silently disabling the feature.
      enabled = localStorage.getItem(STORE_KEY) !== "off";
    } catch {
      enabled = true;
    }
  }
  return enabled;
}

/** Turn site icons on or off. Turning them OFF forgets what was already
 * fetched, so the room stops showing icons it is no longer allowed to ask
 * for — a setting that only applied to future sites would be a half-truth. */
export function setSiteIconsEnabled(on: boolean): void {
  if (siteIconsEnabled() === on) return;
  enabled = on;
  try {
    localStorage.setItem(STORE_KEY, on ? "on" : "off");
  } catch {
    // storage refused; the choice holds for this run
  }
  if (!on) {
    known.clear();
    results.clear();
  }
  announce();
}

// ── the shared per-host cache ─────────────────────────────────────────────

/** Settled answers, by host: a `data:` URL, or null for every honest no.
 * The shell caches across launches; this caches across components, so a
 * ledger of forty web lines makes one ask per site and not one per line. */
const known = new Map<string, string | null>();
/** One stable `ready` value per host. `useSyncExternalStore` compares
 * snapshots by identity, so a fresh object per read would render forever. */
const results = new Map<string, FaviconResult>();
/** Hosts currently being asked about. */
const pending = new Set<string>();
/** Every mounted `useFavicon`, so a settled answer redraws the lines waiting
 * on it and a changed setting redraws all of them. */
const listeners = new Set<() => void>();

function announce(): void {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function ask(host: string): void {
  if (known.has(host) || pending.has(host)) return;
  const shell = desktopHost();
  // A plain browser tab has no bridge, so there is nothing to ask — and
  // neither has a shell whose preload predates this method, which a running
  // app can genuinely have. Both are the globe, permanently, and both are
  // stated rather than retried.
  if (shell === null || typeof shell.favicon !== "function") {
    known.set(host, null);
    return;
  }
  pending.add(host);
  shell
    .favicon(host)
    .then((src) => {
      known.set(host, typeof src === "string" && src !== "" ? src : null);
    })
    .catch(() => {
      // The shell refused or went away. Same fact to a reader: the globe.
      known.set(host, null);
    })
    .finally(() => {
      pending.delete(host);
      announce();
    });
}

/** The host a source url belongs to, or null when it names none. Lowercased,
 * because a host is case-insensitive and the cache is not. */
export function hostOf(url: string): string | null {
  if (typeof url !== "string" || url === "") return null;
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return null;
    // One trailing dot is legal FQDN syntax naming the same host; `URL`
    // keeps it, and it would otherwise be a second cache entry.
    const host = parsed.hostname.toLowerCase().replace(/\.$/, "");
    return host === "" ? null : host;
  } catch {
    return null;
  }
}

// ── the hook ──────────────────────────────────────────────────────────────

/** The setting, live. A toggle binds to this; so does every icon in the
 * room, which is how turning it off empties them in the same frame. */
export function useSiteIconsEnabled(): boolean {
  return useSyncExternalStore(subscribe, siteIconsEnabled, siteIconsEnabled);
}

/**
 * One host's icon.
 *
 * Returns `off` when the setting is off, `loading` while the shell is being
 * asked, `ready` with a `data:` URL when there is one, and `globe` for every
 * no — a host with no icon, a refusal, a 404, a browser tab with no bridge.
 * A null host is a globe too, which is what a source with no parseable url
 * honestly is.
 *
 * The ask happens during the subscribe, not in an effect: several lines
 * naming one site share the module-level entry, so the request is made once
 * regardless of how many of them mount. Reading the setting through its own
 * hook is what makes a switch back ON re-ask — the subscription itself is
 * keyed on the setting, so React re-subscribes and the ask runs again.
 */
export function useFavicon(host: string | null): FaviconResult {
  const on = useSiteIconsEnabled();

  const subscribeToHost = useCallback(
    (listener: () => void) => {
      if (host !== null && on) ask(host);
      return subscribe(listener);
    },
    [host, on],
  );

  const snapshot = useCallback((): FaviconResult => {
    if (!on) return OFF;
    if (host === null) return GLOBE;
    const src = known.get(host);
    if (src === undefined) return LOADING;
    // Frozen results, not fresh objects: `useSyncExternalStore` compares by
    // identity and a new object every read is an infinite render.
    return src === null ? GLOBE : resultFor(host, src);
  }, [host, on]);

  return useSyncExternalStore(subscribeToHost, snapshot, snapshot);
}

function resultFor(host: string, src: string): FaviconResult {
  const existing = results.get(host);
  if (existing !== undefined && existing.src === src) return existing;
  const made: FaviconResult = { src, state: "ready" };
  results.set(host, made);
  return made;
}
