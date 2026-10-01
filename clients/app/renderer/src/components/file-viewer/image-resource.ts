import { useEffect, useState } from "react";
import type { RefObject } from "react";

export function imageRequestUrl(url: string, attempt = 0): string {
  if (attempt === 0) return url;
  const value = new URL(url, window.location.href);
  value.searchParams.set("retry", String(attempt));
  return value.toString();
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
