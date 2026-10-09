// The reader: one session's transcript, read-only, landed on a line — what a
// host shows BEFORE it opens the room there (VibeField's Peek). It reads
// through the same ports as the room and draws the room's own document, with
// nothing around it: no head band, no acts, no dialogs, no keys. It writes
// nothing back to any room; a host that wants the session opened opens the
// room at the same place (`HistoryRoomHandle.openAt`).
//
// The landing stays marked rather than fading, because here it is the point:
// the line, and the words the search matched in it. How the mark looks is the
// host's (three tokens, `--ah-match`, `--ah-match-weight` and
// `--ah-match-line`, set on any element around the reader); unset, it is the
// room's own neutral match.
import { useCallback, useMemo, useState } from "react";
import type { RoomPorts } from "./ports";
import type { KeyValueStore } from "./lib/room-storage";
import { RoomProvider, useRoom } from "./room-context";
import { TranscriptColumn } from "./components/Transcript";
import type { ThreadFocus } from "./components/Transcript";
import { MatchTermsProvider, ReaderModeProvider, matchTermsOf } from "./components/transcript/reader-context";
import { EmptyState } from "./components/ui";
import { useMeta, useTimeline } from "./lib/hooks";

/** Where the reader opens: a session, one of its messages, and the words
 * that matched there. */
export interface ReaderAddress {
  readonly sessionId: string;
  /** The line to land on and keep marked; absent, the session's latest. */
  readonly timelineId?: string | null | undefined;
  /** The search that found it: its words are marked in the prose. */
  readonly query?: string | null | undefined;
}

export interface TranscriptReaderProps extends RoomPorts {
  /** Read by value: a new session or line lands again; the same one does not. */
  readonly at: ReaderAddress;
  /** As the room's: `paper` paints the room's ground; `host` paints none. */
  readonly surface?: "paper" | "host" | undefined;
  /** As the room's: the fold preference and the site icons are kept here. */
  readonly storagePrefix?: string | undefined;
  readonly storage?: KeyValueStore | null | undefined;
}

/** One session's transcript, read-only, at a line. */
export function TranscriptReader(props: TranscriptReaderProps) {
  const { catalog, sessions, assets, host } = props;
  const ports = useMemo<RoomPorts>(
    () => ({ catalog, sessions, assets, ...(host === undefined ? {} : { host }) }),
    [catalog, sessions, assets, host],
  );
  return (
    <RoomProvider ports={ports} options={{ storagePrefix: props.storagePrefix, storage: props.storage }}>
      <ReaderBody at={props.at} surface={props.surface ?? "paper"} />
    </RoomProvider>
  );
}

function focusOf(at: ReaderAddress): ThreadFocus | null {
  const timelineId = at.timelineId ?? null;
  return timelineId === null ? null : { sessionId: at.sessionId, timelineId };
}

function ReaderBody({ at, surface }: { readonly at: ReaderAddress; readonly surface: "paper" | "host" }) {
  const { catalog, sessions, host } = useRoom();
  const { meta, status } = useMeta(catalog);

  // The thread the session belongs to, as the room opens it: a delegated
  // thread is read inside its root, landed where its line is.
  const target = useMemo(() => meta?.sessions.find((s) => s.id === at.sessionId) ?? null, [meta, at.sessionId]);
  const root = useMemo(() => {
    if (target === null || target.parent_session_id === null) return target;
    return meta?.sessions.find((s) => s.id === target.root_session_id) ?? target;
  }, [meta, target]);
  const feed = useTimeline(sessions, root?.id ?? null);

  // The landing, by value: a new line lands again, a re-render does not.
  const landingKey = `${at.sessionId}\u0000${at.timelineId ?? ""}`;
  const [landing, setLanding] = useState(() => ({ key: landingKey, focus: focusOf(at) }));
  if (landing.key !== landingKey) setLanding({ key: landingKey, focus: focusOf(at) });
  const focus = landing.key === landingKey ? landing.focus : focusOf(at);
  const consumed = useCallback(() => setLanding((current) => ({ ...current, focus: null })), []);

  const terms = useMemo(() => matchTermsOf(at.query), [at.query]);
  const [receipt, setReceipt] = useState<string | null>(null);
  const showReceipt = useCallback((text: string) => {
    setReceipt(text);
    window.setTimeout(() => setReceipt((current) => (current === text ? null : current)), 2600);
  }, []);

  const kindOf = useMemo(() => {
    const byId = new Map((meta?.runtimes ?? []).map((r) => [r.id, r.kind]));
    return (runtimeId: string) => byId.get(runtimeId) ?? runtimeId;
  }, [meta]);
  // As the room reads it: the desktop's home, else one an index root still names.
  const home = useMemo(() => {
    const fromShell = host?.homeDir ?? null;
    if (fromShell !== null) return fromShell;
    const dataRoot = meta?.runtimes[0]?.data_root ?? null;
    if (dataRoot === null || !dataRoot.startsWith("/")) return null;
    return dataRoot.replace(/\/\.[^/]+$/, "");
  }, [meta, host]);

  return (
    <div className="ah-frame ah-reader" data-ah-surface={surface}>
      {root === null ? (
        <EmptyState
          style={{ flex: 1 }}
          title={
            meta === null
              ? status === "closed" ? "The index is not answering" : "Opening the session"
              : status === "live" ? "That session is not in this index" : "Opening the session"
          }
          description={meta === null && status === "closed" ? (catalog.unavailable?.() ?? null) : null}
        />
      ) : (
        <ReaderModeProvider value>
          <MatchTermsProvider value={terms}>
            <TranscriptColumn
              session={root}
              duringBackfill={false}
              sessions={meta?.sessions ?? []}
              project={null}
              feed={feed}
              focus={focus}
              onFocusConsumed={consumed}
              onReceipt={showReceipt}
              emptyPrompt={null}
              stepper={null}
              onStepMatch={() => undefined}
              onDismissStepper={() => undefined}
              getRuntimeKind={kindOf}
              homeDirectory={home}
              reader
            />
          </MatchTermsProvider>
        </ReaderModeProvider>
      )}
      {receipt !== null && <div className="ah-receipt">{receipt}</div>}
    </div>
  );
}
