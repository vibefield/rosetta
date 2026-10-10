// @rosetta/room — rosetta's History room behind injected ports. A host hands
// it a catalog, a way to open one session's body, an asset source and, on a
// desktop, a host port; the room dials nothing itself. See README.md.

export { HistoryRoom } from "./HistoryRoom";
export type {
  HistoryRoomHandle,
  HistoryRoomProps,
  HistoryRoomSearch,
  RoomInset,
  RoomSelection,
  RoomTarget,
} from "./HistoryRoom";

/** What a host may hand the room beyond its ports (all optional). */
export type {
  RoomAct,
  RoomFolder,
  RoomFolderGroup,
  RoomFolders,
  RoomHeadActs,
  RoomHostInputs,
  RoomMenu,
  RoomMenuGroup,
  RoomMenuItem,
  RoomProjectContext,
  RoomProjectRow,
  RoomSection,
  RoomSessionRow,
} from "./host-inputs";

/** The reader (PK3): one session's transcript, read-only, at a line. */
export { TranscriptReader } from "./TranscriptReader";
export type { ReaderAddress, TranscriptReaderProps } from "./TranscriptReader";

export type {
  AssetSource,
  AssetUrl,
  CatalogPort,
  CatalogSearch,
  CatalogStatus,
  Closable,
  CollaborationFeed,
  FigureWindow,
  GitFacts,
  GitWorktree,
  HostPort,
  RoomPorts,
  SessionClient,
  SessionFeed,
  SessionPort,
  WorktreeFileActionRequest,
  WorktreeFileActionResult,
  WorktreeHostEvent,
  WorktreeHostPort,
  WorktreeOpenResult,
} from "./ports";
export type { SummaryTab } from "./room-context";
export type { ProjectStats } from "./lib/model";
export type { KeyValueStore } from "./lib/room-storage";
export { DEFAULT_STORAGE_PREFIX } from "./lib/room-storage";

/** A host that holds rosetta's own StatusResponse reads it with this. */
export { catalogStatusOf } from "./lib/catalog-status";

// Parts a host's own shell may reuse, so it reads like the room it holds.
export { AgentMark, agentLabel } from "./lib/agents";
export { lastActive } from "./lib/format";
export { hasPrimaryModifier, shortcutLabel } from "./components/search/shortcut";
export { MoonGlyph, SunGlyph } from "./lib/icons";
