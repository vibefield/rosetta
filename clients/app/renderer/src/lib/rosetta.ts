// One client for the whole room. The daemon address is localhost by
// default; `localStorage.setItem("ah.daemon", "http://127.0.0.1:PORT")`
// repoints a dev build without a rebuild.
import { RosettaClient } from "@rosetta/protocol";

function baseUrl(): string {
  const fromQuery = new URLSearchParams(window.location.search).get("daemon");
  if (fromQuery) return fromQuery;
  try {
    const stored = localStorage.getItem("ah.daemon");
    if (stored) return stored;
  } catch {
    // storage unavailable; use the default
  }
  return "http://127.0.0.1:4640";
}

export const client = new RosettaClient({ baseUrl: baseUrl() });
