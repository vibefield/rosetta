/** Recorded web addresses are data, never relative navigation or executable
 * schemes. Keep the exact URL for opening; only normalize the icon cache key. */
export function webAddress(value: string | null): { href: string; host: string } | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== "https:" && url.protocol !== "http:") return null;
    if (url.username || url.password) return null;
    const host = url.hostname.toLowerCase().replace(/\.$/, "");
    return host ? { href: url.href, host } : null;
  } catch {
    return null;
  }
}
