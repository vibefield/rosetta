// Numbers and time, per the design record §4: three significant figures with
// digits and unit in separate slots; every number that can change is tabular.

export interface TokenParts {
  n: string;
  u: string;
}

export function fmtTokens(n: number): TokenParts {
  let v: number;
  let u: string;
  if (n >= 1e9) {
    v = n / 1e9;
    u = "B";
  } else if (n >= 1e6) {
    v = n / 1e6;
    u = "M";
  } else if (n >= 1e3) {
    v = n / 1e3;
    u = "K";
  } else {
    return { n: String(n), u: "" };
  }
  return { n: v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2), u };
}

export function tokenText(n: number): string {
  const t = fmtTokens(n);
  return t.n + t.u;
}

/** `now` under 90s, then minutes, hours, `yesterday`, days. */
export function lastActive(ms: number, nowMs: number): string {
  const delta = Math.max(0, nowMs - ms);
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

const DAY_MS = 86_400_000;

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** `Today`, `Yesterday`, then `Mon 24 Aug` — the session list's day breaks. */
export function dayLabel(ms: number, nowMs: number): string {
  const today = startOfDay(nowMs);
  const day = startOfDay(ms);
  if (day >= today) return "Today";
  if (day >= today - DAY_MS) return "Yesterday";
  const d = new Date(ms);
  return d.toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short" });
}

/** Home-relative form of a path, the way the ledger row prints one. */
export function tildePath(path: string, home: string | null): string {
  if (home && path.startsWith(home)) return `~${path.slice(home.length)}`;
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
