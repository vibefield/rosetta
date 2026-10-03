// The transcript view model — the locked record's document, as pure functions.
//
// Three concerns, three modules, one public door:
//
//   · `ledger`    — §04's twenty-eight families as `verb · object · result`,
//                   folded by class (T-3) and hued only by what did not
//                   complete (T-5).
//   · `turns`     — §03's anatomy: one prompt, one head, ordered segments.
//   · `sittings`  — §07's day and pause cuts, one pill carrying both.
//
// Everything here is pure: no IO, no clock read, no randomness. `now` is always
// a parameter, so a test can name the day it is testing.
export type {
  LedgerClass,
  LedgerContext,
  LedgerGlyph,
  ObjectPart,
  ObjectTone,
  ToolFamily,
} from "./vocabulary";
export {
  FAMILY_CLASS,
  FAMILY_GLYPH,
  FAMILY_NOUN,
  FOLD_NOUN,
  attachmentLabel,
  basename,
  canonicalText,
  citationLabel,
  familyNoun,
  firstLine,
  noticeText,
  objectText,
  pathParts,
  resourceDisplay,
  resourceLabel,
  shortenPath,
  spanText,
  splitPath,
  urlHost,
} from "./vocabulary";

export type {
  CommandFact,
  CommandDirectoryContext,
  DelegateFact,
  DetailKind,
  Diffstat,
  FamilyProjection,
  FetchFact,
  LedgerDetail,
  PathFact,
  SourceFact,
  TaskChange,
  WorkflowAction,
  WorkflowFact,
} from "./families";
export {
  COMMAND_PREVIEW_CHARACTER_LIMIT,
  TASK_PREVIEW_LIMIT,
  commandDirectoryContext,
  commandPreviewText,
  diffstatText,
  projectAction,
  taskDisplayRows,
} from "./families";

export type {
  LedgerLine,
  LedgerState,
  ResultTone,
  RunNoun,
  RunOther,
  RunSummary,
} from "./ledger";
export {
  FOLD_NAME_LIMIT,
  RUN_FOLD_THRESHOLD,
  commandName,
  ledgerLine,
  ledgerLines,
  linkedChildSessionIds,
  runSummary,
} from "./ledger";

export type {
  AttachmentFact,
  DocumentOptions,
  LedgerMember,
  LocalSetting,
  Prompt,
  ResponseHead,
  Segment,
  Turn,
} from "./turns";
export { documentTurns, attachmentMentions, localSetting, readAttachments } from "./turns";

export type { DayEntry, Sitting } from "./sittings";
export {
  PROMPT_CLUSTER_MS,
  SITTING_PAUSE_MS,
  calendarDayKey,
  dayList,
  sittingCount,
  sittingDayLabel,
  sittings,
  withinPromptCluster,
} from "./sittings";

export type {
  CollaborationStamp,
  CollaborationTimeBasis,
  RuntimeProfile,
  RuntimeProfileBasis,
  TeamMemberOperation,
  TeamMemberPresentation,
  TeamParticipantPresentation,
  TeamReportWork,
  TeamChronology,
  WorkerThreadLinks,
} from "./collaboration";
export {
  isWorkflowOwnedSession,
  standaloneWorkerSessions,
} from "./collaboration";

export type {
  WorkflowAttemptFact,
  WorkflowAttemptIndex,
  WorkflowRuntimeFact,
} from "./workflow";
export {
  workflowAttemptIndex,
  workflowAttemptsFor,
  workflowRuntime,
  workflowWorkerState,
} from "./workflow";
export {
  collaborationStamp,
  interleaveTeamCollaboration,
  participantPresentation,
  runtimeProfile,
  teamMemberPresentations,
  teamChronology,
  teamParticipants,
  taskBoundedReportWork,
  teamSessionIds,
  workerThreadLinks,
} from "./collaboration";
