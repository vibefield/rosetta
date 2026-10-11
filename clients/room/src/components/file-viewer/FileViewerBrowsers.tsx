import { useEffect, useRef, useState } from "react";
import type { HostPort, InstalledBrowser } from "../../ports";
import { ExternalGlyph } from "../../lib/icons";

type BrowserList =
  | { status: "loading" }
  | { status: "failed" }
  | { status: "ready"; browsers: readonly InstalledBrowser[] };

/** Browser discovery and launches stay with the host. This component mounts
 * only for a browser-readable file, so other viewers never scan the machine. */
export function FileViewerBrowsers({ name, bytes, listBrowsers, openInBrowser, onReceipt }: {
  name: string;
  bytes: Uint8Array | null;
  listBrowsers: HostPort["browsers"] | undefined;
  openInBrowser: HostPort["openInBrowser"];
  onReceipt: ((text: string) => void) | undefined;
}) {
  const [list, setList] = useState<BrowserList>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);
  const [pending, setPending] = useState<string | null>(null);
  const launching = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  useEffect(() => {
    if (!listBrowsers) return;
    let alive = true;
    let generation = 0;
    const refresh = async () => {
      const current = ++generation;
      try {
        const browsers = await listBrowsers();
        if (alive && current === generation) setList({ status: "ready", browsers });
      } catch {
        if (alive && current === generation) setList({ status: "failed" });
      }
    };
    setList({ status: "loading" });
    void refresh();
    const focus = () => { void refresh(); };
    window.addEventListener("focus", focus);
    return () => { alive = false; window.removeEventListener("focus", focus); };
  }, [listBrowsers, attempt]);

  const open = async (browser?: InstalledBrowser) => {
    if (bytes === null || launching.current) return;
    launching.current = true;
    setPending(browser?.id ?? "default");
    const label = browser?.name ?? "your browser";
    let opened = false;
    try { opened = await openInBrowser(name, bytes, browser?.id); } catch { /* reported below */ }
    if (mounted.current) {
      launching.current = false;
      setPending(null);
      onReceipt?.(opened ? `opened in ${label}` : `could not open in ${label}`);
    }
  };

  // Keep the existing capability for embedded hosts that do not enumerate
  // browsers. A host returning an empty list never invents a default button.
  if (!listBrowsers) return <button className="ah-iconbtn" type="button"
    title="Open in browser" aria-label="Open in browser" disabled={bytes === null || pending !== null}
    onClick={() => { void open(); }}><ExternalGlyph /></button>;

  return <div className="ah-fileview__browsers" role="group" aria-label="Open in browser">
    {list.status === "loading" ? <span className="ah-fileview__browser-status" role="status">Finding browsers…</span>
      : list.status === "failed" ? <button className="ah-iconbtn" type="button"
          title="Could not find browsers. Retry" aria-label="Retry browser detection"
          onClick={() => setAttempt(value => value + 1)}><ExternalGlyph /></button>
      : list.browsers.length === 0 ? <span className="ah-fileview__browser-status" role="status">No supported browsers found</span>
      : list.browsers.map(browser => <button className="ah-iconbtn ah-fileview__browser" type="button"
          key={browser.id} data-browser-id={browser.id}
          title={`Open in ${browser.name}`} aria-label={`Open in ${browser.name}`}
          aria-busy={pending === browser.id} disabled={bytes === null || pending !== null}
          onClick={() => { void open(browser); }}>
          {browser.icon ? <img src={browser.icon} alt="" draggable={false} />
            : <span className="ah-fileview__browser-fallback" aria-hidden="true">{browser.name.charAt(0)}</span>}
        </button>)}
  </div>;
}
