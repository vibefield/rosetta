// The whole room, mounted the way a host mounts it: HistoryRoom over the
// package's demo ports, with no daemon and no desktop bridge. Each scenario
// wraps those ports the way a host might — late, fetching, refusing, or
// absent — and the driver reads what the room asked of them.
import { useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { HistoryRoom } from "../renderer/src/HistoryRoom";
import { createDemoPorts, createRoomDemo } from "../renderer/src/fixtures";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/room.css";
import "../renderer/src/styles/ingest.css";
import "../renderer/src/styles/transcript.css";
import "../renderer/src/styles/search.css";

const demo = createRoomDemo();
const firstPrompt = demo.timelines.get("s0")[0];
const [imageA, imageB] = firstPrompt.body.attachments.map((attachment) => attachment.source.asset.digest.slice(0, 4));

export const log = { opened: [], closed: [], images: [], attachments: [], favicons: [], selections: [] };

const icon = `data:image/svg+xml,${encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16" rx="3" fill="#4d79c7"/></svg>')}`;
/** A desktop without a worktree engine: home, platform, browser, icons. */
const desktop = {
  homeDir: "/Users/demo",
  platform: "darwin",
  openInBrowser: async () => true,
  favicon: async (site) => { log.favicons.push(site); return icon; },
};
/** A desktop that also runs a worktree engine, which never reports. */
const engineDesktop = {
  ...desktop,
  worktree: {
    open: async () => ({ ok: true, generation: 1 }),
    gitFacts: async () => null,
    fileAction: async () => ({ ok: true }),
    ports: () => () => {},
  },
};

function ports(scenario) {
  const base = createDemoPorts(demo);
  const catalog = { ...base.catalog };
  if (scenario === "disconnected") {
    catalog.subscribeMeta = (handlers) => { queueMicrotask(() => handlers.onStatus?.("closed")); return { close() {} }; };
    catalog.status = () => new Promise(() => {});
  }
  if (scenario === "consent" || scenario === "consent-default") {
    catalog.subscribeMeta = (handlers) => {
      queueMicrotask(() => { handlers.onStatus?.("live"); handlers.onChange?.({ runtimes: [], projects: [], sessions: [] }, "snapshot"); });
      return { close() {} };
    };
    catalog.status = async () => ({ ...demo.status, canonicalReady: false, ingest: { ...demo.status.ingest, phase: "awaiting_consent" } });
  }
  if (scenario === "late") {
    // The index delivers its rows a beat after the room mounts.
    catalog.subscribeMeta = (handlers) => {
      setTimeout(() => { handlers.onStatus?.("live"); handlers.onChange?.(demo.meta, "snapshot"); }, 250);
      return { close() {} };
    };
  }
  const svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 3"><rect width="4" height="3" fill="#6366f1"/></svg>';
  return {
    ...(scenario === "host" ? { host: desktop } : scenario === "narrow-host" || scenario === "wide-host" ? { host: engineDesktop } : {}),
    catalog,
    sessions: {
      open(id) { log.opened.push(id); return base.sessions.open(id); },
      close(id) { log.closed.push(id); base.sessions.close(id); },
    },
    assets: {
      imageAssetUrl(asset, variant) {
        const name = `${asset.digest.slice(0, 4)}:${variant}`;
        log.images.push(name);
        if (scenario !== "fetching") return base.assets.imageAssetUrl(asset, variant);
        // A credentialed host: bytes first, then an object URL. The first
        // ask for the second image is refused, as a failed fetch would be.
        const refused = name.startsWith(imageB) && log.images.filter((n) => n === name).length === 1;
        return new Promise((resolve) => setTimeout(() => resolve(refused ? null
          : URL.createObjectURL(new Blob([svg], { type: "image/svg+xml" }))), 120));
      },
      attachmentAssetUrl(asset) { log.attachments.push(asset.digest.slice(0, 4)); return base.assets.attachmentAssetUrl(asset); },
    },
  };
}

/** A host's own store, recording every name the room writes into it. */
const recorded = new Map();
const recordingStore = { getItem: (key) => recorded.get(key) ?? null, setItem: (key, value) => { recorded.set(key, value); } };

function Fixture() {
  const [scenario, setScenario] = useState(null);
  const [sheetOpen, setSheetOpen] = useState(false);
  const room = useRef(null);
  window.roomFixture = {
    log, imageA, imageB,
    mount(name) {
      for (const list of Object.values(log)) list.length = 0;
      flushSync(() => setScenario({ name, key: Math.random(), ports: ports(name) }));
    },
    openAt(target) { room.current.openAt(target); },
    stored: () => [...recorded.keys()].sort(),
  };
  if (scenario === null) return null;
  const { name, key, ports: p } = scenario;
  // What a host paints beneath a room that asked it to: here, a loud ground,
  // so a capture shows whether anything of the room's paper is left.
  document.body.style.background = name === "glass"
    ? "repeating-linear-gradient(45deg, #2b4a8a 0 40px, #8a2b5e 40px 80px)" : "";
  // A host may seat the room in less than its window.
  const width = name === "narrow-host" ? 1200 : name === "wide-host" ? 1400 : null;
  return <div style={{ height: "100%", ...(width === null ? {} : { width }) }}><HistoryRoom key={key} ref={room} {...p}
    onSelect={(selection) => log.selections.push(selection)}
    {...(name === "sized" ? { edgeInset: { top: 12, right: 8, bottom: 8, left: 8 }, scrollFoot: "48px", floatingFoot: "40px" } : {})}
    {...(name === "search" || name === "prefixed" ? { search: { open: sheetOpen, onOpenChange: setSheetOpen, accessory: <span data-accessory="">host</span> } } : {})}
    {...(name === "prefixed" ? { storagePrefix: "vf.history.", storage: recordingStore } : {})}
    {...(name === "disconnected" ? { unavailable: <span data-host-words="">The index is resting.</span> } : {})}
    {...(name === "consent" ? { consent: <div data-consent="">Choose your agents</div> } : {})}
    {...(name === "glass" ? { surface: "host", portalContainer: document.getElementById("host-layer") } : {})}
  /></div>;
}

createRoot(document.getElementById("root")).render(<Fixture />);
