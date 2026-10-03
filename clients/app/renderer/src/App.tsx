// rosetta's app: the History room's reference host. It owns what belongs to a
// window and not to the room — the client and the daemon it points at, the
// keys, the URL's parameters, the theme, the consent screen, and the words for
// a daemon that is not there — and hands the room its ports.
import { useEffect, useRef, useState } from "react";
import type { RosettaClient } from "@rosetta/protocol";
import { HistoryRoom, hasPrimaryModifier } from "@rosetta/room";
import type { HistoryRoomHandle } from "@rosetta/room";
import { RollCallPanel } from "./components/RollCall";
import { ThemeToggle } from "./components/ThemeToggle";
import type { RosettaHost } from "./host/rosetta-host";

function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

/** Where the room looked, and how to start a daemon there. Follows the
 * client's address as the shell repoints it. */
function DaemonHint({ client }: { readonly client: RosettaClient }) {
  const [, setRevision] = useState(0);
  useEffect(() => client.onConnectionChange(() => setRevision((value) => value + 1)), [client]);
  return (
    <>
      Looking for the daemon at <code>{client.baseUrl}</code>. Start it with{" "}
      <code>rosettad serve --db &lt;projection&gt;</code> and this room fills in on
      its own.
    </>
  );
}

export default function App({ host }: { readonly host: RosettaHost }) {
  const room = useRef<HistoryRoomHandle>(null);
  const [sheetOpen, setSheetOpen] = useState(false);
  const [params] = useState(() => new URLSearchParams(window.location.search));

  // ⌘K summons the sheet from anywhere; `/` does the same when nothing is
  // being typed; ⌘G / ⇧⌘G walk the landed session's matches.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.altKey) return;
      const primary = hasPrimaryModifier(event);
      if (primary && (event.key === "k" || event.key === "K")) {
        event.preventDefault();
        setSheetOpen((open) => !open);
        return;
      }
      if (primary && !sheetOpen && (event.key === "g" || event.key === "G")) {
        if (room.current?.stepMatch(event.shiftKey ? -1 : 1)) event.preventDefault();
        return;
      }
      if (
        event.key === "/" &&
        !primary &&
        !sheetOpen &&
        !isTypingTarget(event.target) &&
        !document.body.querySelector("[data-ah-fileview]")
      ) {
        event.preventDefault();
        setSheetOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sheetOpen]);

  return (
    <HistoryRoom
      ref={room}
      {...host.ports}
      search={{
        open: sheetOpen,
        onOpenChange: setSheetOpen,
        initialQuery: params.get("q") ?? "",
        accessory: <ThemeToggle />,
      }}
      initialSummaryTab={params.get("tab") === "models" ? "models" : "activity"}
      unavailable={<DaemonHint client={host.client} />}
      consent={<RollCallPanel client={host.client} />}
    />
  );
}
