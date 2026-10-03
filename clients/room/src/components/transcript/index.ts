// The transcript's public door. `components/Transcript.tsx` keeps the column
// itself (its props are App.tsx's contract); everything the column is made of
// lives here.
export { Anchor } from "./Anchor";
export { DayMenu, DayPill, Sitting, markStuckPills, pillText } from "./DayPill";
export { TranscriptDocument } from "./Document";
export type { TranscriptDocumentProps } from "./Document";
export { useTranscriptModel } from "./document-model";
export type { TranscriptModel } from "./document-model";
export { documentPlan } from "./document-plan";
export type { PlannedSitting, TurnPart } from "./document-plan";
export { Favicon, FaviconStack } from "./Favicon";
export { useFoldPreference } from "./fold-preference";
export { ActivityGlyph, FamilyGlyph, FailGlyph, LineChevron } from "./glyphs";
export { HeadOfDocument, TopFade, sessionStartLabel } from "./HeadOfDocument";
export { Ledger, Line } from "./Ledger";
export { LineDetail, hasDetail } from "./LineDetail";
export { GapMarker, Marker, Watermark } from "./Marker";
export { Prompt } from "./Prompt";
export { Prose, SegmentActs } from "./Prose";
export { ReceiptPill } from "./ReceiptPill";
export { ResponseHead } from "./ResponseHead";
export { Subthread } from "./Subthread";
export { Thinking } from "./Thinking";
export {
  buildThreadIndex,
  messageDomId,
  sessionContains,
  subthreadTitle,
} from "./thread-index";
export type { ThreadFocus, ThreadIndex } from "./thread-index";
