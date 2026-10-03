import { useEffect, useMemo, useState } from "react";
import type { RefObject } from "react";
import type { ImageRendition, ImageResource, ImageUrl } from "./types";

/** A retry's address. A daemon URL gets a cache-busting parameter; an object
 * or data URL cannot carry one, so it is used as it is (its source was asked
 * again for the retry). */
export function imageRequestUrl(url: string, attempt = 0): string {
  if (attempt === 0) return url;
  const value = new URL(url, window.location.href);
  if (value.protocol !== "http:" && value.protocol !== "https:") return url;
  value.searchParams.set("retry", String(attempt));
  return value.toString();
}

export interface ResolvedImageUrl {
  /** The address to load, or null while pending or when there is none. */
  readonly url: string | null;
  /** The source is still fetching this rendition. */
  readonly pending: boolean;
}

const NONE: ResolvedImageUrl = { url: null, pending: false };

/** One rendition's address, asked of its source once per image, rendition and
 * attempt. A source that answers at once answers in this render, exactly as a
 * plain string did; one that must fetch first answers when it has the bytes. */
export function useImageUrl(
  resource: ImageResource | null | undefined,
  rendition: ImageRendition,
  attempt = 0,
): ResolvedImageUrl {
  const key = resource ? `${resource.key}\u0000${rendition}\u0000${attempt}` : null;
  // Keyed by identity, not by the resource object: a parent may rebuild the
  // resource every render, and a fetching source must not be asked each time.
  const answer = useMemo<ImageUrl>(() => (resource ? resource.url(rendition) : null), [key]);
  const [settled, setSettled] = useState<{ readonly key: string; readonly url: string | null } | null>(null);
  useEffect(() => {
    if (key === null || answer === null || typeof answer === "string") return;
    let live = true;
    Promise.resolve(answer).then(
      (url) => {
        if (live) setSettled({ key, url });
      },
      () => {
        if (live) setSettled({ key, url: null });
      },
    );
    return () => {
      live = false;
    };
  }, [key, answer]);
  if (key === null || answer === null) return NONE;
  if (typeof answer === "string") return { url: imageRequestUrl(answer, attempt), pending: false };
  return settled?.key === key ? { url: settled.url, pending: false } : { url: null, pending: true };
}

/** Stop retaining image elements outside the reading window, including after
 * a horizontal rail was scrolled through. Browser caching owns reusable bytes. */
export function useImageVisibility(ref: RefObject<HTMLElement | null>, rail = false) {
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    const observer = new IntersectionObserver(entries => {
      setVisible(entries.some(entry => entry.isIntersecting));
    }, { root: rail ? node.closest(".ah-image-gallery__rail") : null, rootMargin: rail ? "80px" : "240px" });
    observer.observe(node);
    return () => observer.disconnect();
  }, [ref, rail]);
  return visible;
}
