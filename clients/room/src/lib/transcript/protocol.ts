// Protocol types the `@rosetta/protocol` barrel does not re-export yet.
//
// Every alias here is DERIVED from a union the barrel does export, never
// hand-written: `FileDelta` is literally the element type of the `file_change`
// action's `changes`, so it cannot drift from the ts-rs output and this module
// is not a second copy of the wire contract. Iron rule 6 — parsers exist once,
// in Rust; consumers get generated types.
//
// If `clients/ts/index.ts` grows these exports, delete the matching lines here
// and import them from `@rosetta/protocol` instead. Nothing else changes.
import type { ToolAction, ToolInteraction } from "@rosetta/protocol";

type Family<K extends ToolAction["type"]> = Extract<ToolAction, { type: K }>;

export type NativeToolRef = ToolInteraction["native"];
export type ToolFailure = NonNullable<ToolInteraction["failure"]>;
export type ToolTiming = NonNullable<ToolInteraction["timing"]>;

export type FileDelta = Family<"file_change">["changes"][number];
export type FindingItem = Family<"finding">["findings"][number];
export type DisplayField = Family<"external">["fields"][number];
export type DisplayValue = DisplayField["value"];
export type DisplayScalar = Extract<DisplayValue, { type: "list" }>["value"][number];
export type UserQuestion = Family<"question">["questions"][number];
export type UserAnswer = Family<"question">["answers"][number];
export type CollaboratorRef = NonNullable<
  Family<"collaborate">["outcome"]
>["participants"][number];
export type SourceLocation = NonNullable<Family<"code_lookup">["outcome"]>["locations"][number];
export type SourceRange = NonNullable<Family<"file_read">["targets"][number]["range"]>;
export type CanonicalDocument = Family<"structured_output">["document"];
export type WebSource = NonNullable<Family<"web_search">["outcome"]>["sources"][number];
export type FindingSeverity = FindingItem["severity"];
