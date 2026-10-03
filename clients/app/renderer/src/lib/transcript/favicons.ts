// Site icons, as the renderer sees them.
//
// A search's sources are the one place in this room where identity should
// carry colour: a site's own icon is content, and it is the fastest way to
// tell GitHub from MDN from somebody's blog. The icon is not in the protocol
// — a source carries a url and a title and nothing else — so it has to be
// fetched, and fetching is a network act the renderer is not allowed to
// perform. It asks the room's host (`HostPort.favicon`), which validates the
// host, dials the SITE rather than a favicon service, and caches per host.
//
// Because it IS a network act, it is named and it is optional: `fetch site
// icons`, on by default, off in one call. Off means off — no request is made,
// and every site wears the globe.
//
// This module holds the room's half: one store per room (the setting and one
// shared per-host map, so eight lines from one site produce one ask), and the
// hooks the components read. It deliberately holds no component and no markup.

import { useCallback, useSyncExternalStore } from "react";
import { useRoom } from "../../room-context";
import { webAddress } from "./web-address";

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

/** One room's icons: the setting, and one shared per-site map so eight lines
 * from one site produce one ask. */
export interface FaviconStore {
  /** Whether this room may ask its host for site icons. */
  enabled(): boolean;
  /** Turn site icons on or off. Turning them OFF forgets what was already
   * fetched, so the room stops showing icons it is no longer allowed to ask
   * for — a setting that only applied to future sites would be a half-truth. */
  setEnabled(on: boolean): void;
  subscribe(listener: () => void): () => void;
  /** Advances whenever the setting changes. */
  generation(): number;
  ask(host: string): void;
  snapshot(host: string | null): FaviconResult;
}

/**
 * The store over a host's `favicon` act, or over none: a plain browser tab
 * has no host to ask, which is the globe, permanently, and stated rather than
 * retried.
 */
export function createFaviconStore(
  fetchIcon: ((host: string) => Promise<string | null>) | null,
): FaviconStore {
  let enabled: boolean | null = null;
  /** Settled answers, by host: a `data:` URL, or null for every honest no.
   * The shell caches across launches; this caches across components, so a
   * ledger of forty web lines makes one ask per site and not one per line. */
  const known = new Map<string, string | null>();
  /** One stable `ready` value per host. `useSyncExternalStore` compares
   * snapshots by identity, so a fresh object per read would render forever. */
  const results = new Map<string, FaviconResult>();
  /** Hosts currently being asked about. */
  const pending = new Map<string, number>();
  let generation = 0;
  /** Every mounted `useFavicon`, so a settled answer redraws the lines waiting
   * on it and a changed setting redraws all of them. */
  const listeners = new Set<() => void>();

  const announce = () => {
    for (const listener of listeners) listener();
  };

  function isEnabled(): boolean {
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

  function resultFor(host: string, src: string): FaviconResult {
    const existing = results.get(host);
    if (existing !== undefined && existing.src === src) return existing;
    const made: FaviconResult = { src, state: "ready" };
    results.set(host, made);
    return made;
  }

  return {
    enabled: isEnabled,
    setEnabled(on) {
      if (isEnabled() === on) return;
      enabled = on;
      generation += 1;
      try {
        localStorage.setItem(STORE_KEY, on ? "on" : "off");
      } catch {
        // storage refused; the choice holds for this run
      }
      if (!on) {
        known.clear();
        results.clear();
        pending.clear();
      }
      announce();
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    generation: () => generation,
    ask(host) {
      if (known.has(host) || pending.has(host)) return;
      if (fetchIcon === null) {
        known.set(host, null);
        return;
      }
      const requestGeneration = generation;
      pending.set(host, requestGeneration);
      fetchIcon(host)
        .then((src) => {
          if (generation !== requestGeneration) return;
          known.set(host, typeof src === "string" && src !== "" ? src : null);
        })
        .catch(() => {
          if (generation !== requestGeneration) return;
          // The host refused or went away. Same fact to a reader: the globe.
          known.set(host, null);
        })
        .finally(() => {
          if (pending.get(host) === requestGeneration) {
            pending.delete(host);
            announce();
          }
        });
    },
    snapshot(host) {
      if (!isEnabled()) return OFF;
      if (host === null) return GLOBE;
      const src = known.get(host);
      if (src === undefined) return LOADING;
      // Frozen results, not fresh objects: `useSyncExternalStore` compares by
      // identity and a new object every read is an infinite render.
      return src === null ? GLOBE : resultFor(host, src);
    },
  };
}

/** The host a source url belongs to, or null when it names none. Lowercased,
 * because a host is case-insensitive and the cache is not. */
export function hostOf(url: string): string | null {
  return webAddress(url)?.host ?? null;
}

// ── the hooks ─────────────────────────────────────────────────────────────

/** The setting, live. A toggle binds to this; so does every icon in the
 * room, which is how turning it off empties them in the same frame. */
export function useSiteIconsEnabled(): boolean {
  const { favicons } = useRoom();
  return useSyncExternalStore(favicons.subscribe, favicons.enabled, favicons.enabled);
}

/** Turn the room's site icons on or off. */
export function useSetSiteIconsEnabled(): (on: boolean) => void {
  return useRoom().favicons.setEnabled;
}

/**
 * One host's icon.
 *
 * Returns `off` when the setting is off, `loading` while the host is being
 * asked, `ready` with a `data:` URL when there is one, and `globe` for every
 * no — a host with no icon, a refusal, a 404, a browser tab with no host.
 * A null host is a globe too, which is what a source with no parseable url
 * honestly is.
 *
 * The ask happens during the subscribe, not in an effect: several lines
 * naming one site share the room's entry, so the request is made once
 * regardless of how many of them mount. The setting's generation also keys
 * the subscription: even an off/on pair batched into one render must start
 * a fresh request, since turning off invalidates the previous one.
 */
export function useFavicon(host: string | null): FaviconResult {
  const { favicons } = useRoom();
  const revision = useSyncExternalStore(favicons.subscribe, favicons.generation, favicons.generation);
  const on = favicons.enabled();

  const subscribeToHost = useCallback(
    (listener: () => void) => {
      if (host !== null && on) favicons.ask(host);
      return favicons.subscribe(listener);
    },
    [favicons, host, on, revision],
  );

  const snapshot = useCallback(() => favicons.snapshot(host), [favicons, host, on]);

  return useSyncExternalStore(subscribeToHost, snapshot, snapshot);
}
