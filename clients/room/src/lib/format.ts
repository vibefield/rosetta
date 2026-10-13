// Numbers and time, per the design record §4: three significant figures with
// digits and unit in separate slots; every number that can change is tabular.

export interface CompactNumberParts {
  n: string;
  u: string;
}

const COMPACT_UNITS = ["", "K", "M", "B", "T"] as const;

/** Shared count/token/currency scale. Round before choosing precision and
 * promote a rounded 1,000 into the next unit (999,500 → 1.00M). */
export function fmtCompactNumber(value: number): CompactNumberParts {
  if (!Number.isFinite(value)) return { n: "—", u: "" };
  const magnitude = Math.abs(value);
  if (magnitude < 999.5) return { n: String(value), u: "" };

  // Round in the original unit so decimal ties such as 9,995 do not become
  // 9.99K through the binary representation of a scaled 9.995.
  const precision = 10 ** (Math.floor(Math.log10(magnitude)) - 2);
  let scaled = Math.round(magnitude / precision) * precision;
  let unit = 0;
  while (scaled >= 1e3 && unit < COMPACT_UNITS.length - 1) {
    scaled /= 1e3;
    unit += 1;
  }
  const decimals = Math.max(0, 2 - Math.floor(Math.log10(scaled)));
  return { n: (Math.sign(value) * scaled).toFixed(decimals), u: COMPACT_UNITS[unit]! };
}

export type TokenParts = CompactNumberParts;

export function fmtTokens(n: number): TokenParts {
  return fmtCompactNumber(n);
}

export function tokenText(n: number): string {
  const t = fmtTokens(n);
  return t.n + t.u;
}

/** `now` under 90s, then minutes, hours, `yesterday`, days. */
export function lastActive(ms: number | null, nowMs: number): string {
  if (ms === null || !Number.isFinite(ms)) return "unknown";
  const delta = nowMs - ms;
  if (delta < 0) return "future";
  if (delta < 90_000) return "now";
  const minutes = Math.floor(delta / 60_000);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days === 1) return "yesterday";
  return `${days}d`;
}

export function duration(fromMs: number, toMs: number): string {
  const mins = Math.max(0, Math.round((toMs - fromMs) / 60_000));
  if (mins < 60) return `${mins}m`;
  const h = Math.floor(mins / 60);
  if (h < 48) return `${h}h ${String(mins % 60).padStart(2, "0")}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

export function clock(ms: number): string {
  const d = new Date(ms);
  const p = (v: number) => String(v).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export function clockShort(ms: number): string {
  const d = new Date(ms);
  const p = (v: number) => String(v).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}`;
}

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** `Today`, `Yesterday`, then `Mon 24 Aug` — the session list's day breaks. */
export function dayLabel(ms: number | null, nowMs: number): string {
  if (ms === null || !Number.isFinite(ms)) return "Activity unknown";
  const today = startOfDay(nowMs);
  const day = startOfDay(ms);
  if (day === today) return "Today";
  const yesterday = new Date(today);
  yesterday.setDate(yesterday.getDate() - 1);
  if (day === yesterday.getTime()) return "Yesterday";
  const d = new Date(ms);
  return d.toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short" });
}

/** Home-relative form of a path, the way the ledger row prints one. The
 * home is a folder, so only the home itself and what is under it shorten:
 * `/Users/ab` is not under `/Users/a`. A root home shortens nothing. */
export function tildePath(path: string, home: string | null): string {
  const root = home?.replace(/\/+$/, "") ?? "";
  if (root === "") return path;
  if (path === root) return "~";
  if (path.startsWith(`${root}/`)) return `~${path.slice(root.length)}`;
  return path;
}

/** A file size, at three significant figures — the ledger's rule for a
 * figure, applied to bytes. `812 B` · `4.20 KB` · `1.3 MB` · `212 MB`. */
export function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`;
}
