// The document as turns — §03's anatomy.
//
// A turn is one prompt and everything that answered it until the next prompt.
// T-1: the prompt is the ONLY filled container in the document; the reply is
// bare and the machinery is typography on a rail. T-4: one head per reply, with
// every later segment still pointable-at through its own row.
//
// Group kinds decide what opens a turn (`docs/ARCHITECTURE.md`: every entry
// belongs to a deterministic causal group, and its derivation exists precisely
// so a client never presents an inferred boundary as source evidence):
//
//   · `human_turn`       opens a turn. The row is the prompt.
//   · `parent_invocation` opens a turn, flagged `fromParent` — inside a
//     subthread the objective the parent sent IS the prompt, and it is drawn as
//     one (§03 F draws the child run headed by its own mark and range).
//   · `agent_cycle`      answers. Never opens a turn.
//   · `system_cycle`     answers — a lifecycle record is part of the reply it
//     happened inside, and every Notice is a hairline marker (§03 G).
//   · `synthetic`        answers. The group carries NO native evidence of a
//     boundary (it is the fallback in `crates/core/src/ops.rs`), and on today's
//     daemon it is the commonest kind of all: 2,748 rows across 38 sessions.
//     Cutting a turn on a group the source could not prove would be inventing
//     structure, so these rows keep their ordinal position and nothing more.
//
// One honest override: a `user` body ALWAYS opens a turn whatever its group
// says, because the projection contract is that `user` means human-authored
// prose — tool results, developer prompts and environment injections cannot
// become one. A person speaking is the document's structure.
import type {
  AttachmentKind,
  AttachmentRef,
  CitationRef,
  CollaborationEvent,
  Notice,
  SummaryPurpose,
  TimelineEntry,
} from "@rosetta/protocol";
import type { LedgerLine, RunSummary } from "./ledger.ts";
import { ledgerLines, runSummary } from "./ledger.ts";
import { withinPromptCluster } from "./sittings.ts";
import type { LedgerContext } from "./vocabulary.ts";
import {
  attachmentLabel,
  basename,
  canonicalText,
  citationLabel,
  noticeText,
  stampSpan,
} from "./vocabulary.ts";

/* ── attachments ─────────────────────────────────────────────────────────── */

/**
 * One attachment tile's facts. A `missing` fact is §06's placeholder: the
 * pipeline let an `[Image: …]` sentence through and stored nothing, so the
 * record draws the dashed tile with what it knows instead of the injected
 * prose. Absence is stated, never faked.
 */
export interface AttachmentFact {
  readonly key: string;
  readonly kind: AttachmentKind;
  readonly name: string | null;
  readonly mediaType: string | null;
  readonly byteLen: number | null;
  readonly width: number | null;
  readonly height: number | null;
  readonly missing: boolean;
  /** The stored reference; null for a placeholder recovered from prose. */
  readonly ref: AttachmentRef | null;
  /** `2048×1536 · JPEG` — the tile's caption, empty when nothing is known. */
  readonly caption: string;
}

// Claude Code writes two different things ahead of pasted images. A numbered
// marker (`[Image #4]`) identifies bytes that did arrive and stays in the
// prose as a reader-facing link. A descriptive marker (`[Image: source: …]`
// or `[Image: original 2400x1600, …]`) is injected transport; when no bytes
// arrived it becomes the honest missing-image fact instead.
const IMAGE_MARKER = /^\s*\[Image(?:\s*#(\d+))?(?::\s*([^\]]*))?\]\s*/;
const NUMBERED_IMAGE_MARKER = /\[Image\s*#(\d+)\]/g;

function mediaShortName(mediaType: string | null, name: string | null): string | null {
  if (mediaType !== null && mediaType !== "") {
    const subtype = mediaType.split("/").at(-1) ?? mediaType;
    return subtype.replace(/^x-/, "").toUpperCase();
  }
  const extension = name === null ? null : /\.([A-Za-z0-9]{1,5})$/.exec(name);
  if (extension === null) return null;
  const upper = extension[1].toUpperCase();
  return upper === "JPG" ? "JPEG" : upper;
}

function caption(width: number | null, height: number | null, type: string | null): string {
  const size = width !== null && height !== null ? `${width}×${height}` : null;
  return [size, type].filter((piece): piece is string => piece !== null).join(" · ");
}

function factFromRef(ref: AttachmentRef, index: number): AttachmentFact {
  const name = attachmentLabel(ref);
  const type = mediaShortName(ref.media_type, name);
  return {
    key: `att-${index}`,
    kind: ref.kind,
    name,
    mediaType: ref.media_type,
    byteLen: ref.byte_len,
    width: ref.dimensions?.width ?? null,
    height: ref.dimensions?.height ?? null,
    missing: false,
    ref,
    caption: caption(ref.dimensions?.width ?? null, ref.dimensions?.height ?? null, type),
  };
}

function factFromMarker(payload: string | null, index: number): AttachmentFact {
  const source = payload === null ? null : /source:\s*(\S+)/.exec(payload);
  const size = payload === null ? null : /(\d{2,5})\s*[x×]\s*(\d{2,5})/.exec(payload);
  const path = source === null ? null : source[1];
  const name = path === null ? null : basename(path);
  const width = size === null ? null : Number(size[1]);
  const height = size === null ? null : Number(size[2]);
  const type = mediaShortName(null, name);
  return {
    key: `att-missing-${index}`,
    kind: "image",
    name,
    mediaType: null,
    byteLen: null,
    width,
    height,
    missing: true,
    ref: null,
    caption: caption(width, height, type),
  };
}

/**
 * Recover attachment facts from a prompt. A numbered marker whose row carries
 * stored bytes remains in the prose so it can open the same viewer as its
 * tile. Descriptive transport markers are removed; when the row carried no
 * attachment, each becomes a placeholder so the reader sees a missing image
 * rather than a sentence about one.
 */
export function readAttachments(
  text: string | null,
  refs: readonly AttachmentRef[],
): { text: string | null; attachments: AttachmentFact[] } {
  const stored = refs.map(factFromRef);
  const markers: (string | null)[] = [];
  let rest = text ?? "";
  for (;;) {
    const match = IMAGE_MARKER.exec(rest);
    if (match === null) break;
    // This is the useful handle the user asked to keep. Leaving the whole
    // match intact also preserves punctuation and spacing exactly as written.
    if (match[1] !== undefined && stored.length > 0) break;
    markers.push(match[2] ?? null);
    rest = rest.slice(match[0].length);
  }
  const attachments =
    stored.length > 0 ? stored : markers.map((payload, index) => factFromMarker(payload, index));
  const trimmed = rest.trim();
  return { text: trimmed === "" ? null : rest, attachments };
}

/**
 * Which stored image each numbered marker names.
 *
 * Claude normally numbers images from one, but persisted sessions can carry
 * global numbers such as `#4` in a prompt containing only one or two images.
 * Direct local numbering wins; otherwise occurrence order pairs the markers
 * with the stored images. A lone stored image is unambiguous at any number.
 */
export function imageMarkerTargets(
  text: string,
  attachments: readonly AttachmentFact[],
): ReadonlyMap<string, AttachmentFact> {
  const images = attachments.filter((fact) => fact.kind === "image" && !fact.missing);
  const targets = new Map<string, AttachmentFact>();
  if (images.length === 0) return targets;

  const markers: RegExpMatchArray[] = [];
  const seen = new Set<string>();
  for (const match of text.matchAll(NUMBERED_IMAGE_MARKER)) {
    if (seen.has(match[0])) continue;
    seen.add(match[0]);
    markers.push(match);
  }
  const locallyNumbered = markers.every((match) => {
    const index = Number(match[1]) - 1;
    return index >= 0 && index < images.length;
  });
  markers.forEach((match, occurrence) => {
    const target =
      images.length === 1
        ? images[0]
        : images[locallyNumbered ? Number(match[1]) - 1 : occurrence];
    if (target !== undefined) targets.set(match[0], target);
  });
  return targets;
}

/* ── the prompt ──────────────────────────────────────────────────────────── */

// A local command as the reader typed it: one line, a leading slash, a name.
const LOCAL_COMMAND = /^\/[A-Za-z][\w:.-]*(?:\s+\S.*)?$/;

export interface LocalSetting {
  readonly kind: "model" | "effort";
  readonly label: "Model" | "Effort";
  readonly value: string;
}

export interface Prompt {
  readonly key: string;
  readonly row: TimelineEntry;
  /** The prompt row plus an absorbed `command_output` notice — the pair is one
   * object (§03 H), so the marker never draws twice. */
  readonly rows: readonly TimelineEntry[];
  readonly at: number | null;
  readonly text: string | null;
  readonly attachments: readonly AttachmentFact[];
  /** Draw a pill, not a panel: this prompt is a local command. */
  readonly isCommand: boolean;
  readonly command: string | null;
  /** The command's reply, riding beside the pill in the caption ramp. */
  readonly commandOutput: string | null;
  readonly commandOutputRow: TimelineEntry | null;
  /** A successful local setting change, reduced to label + canonical value. */
  readonly setting: LocalSetting | null;
  /** The parent session sent this, not a person (`parent_invocation`). */
  readonly fromParent: boolean;
  /** Machine-delivered context has prompt semantics but never human styling. */
  readonly machine: MachinePrompt | null;
}

export interface MachinePrompt {
  readonly label: string;
  readonly sender: string | null;
}

function looksLocalCommand(text: string | null): boolean {
  if (text === null) return false;
  const trimmed = text.trim();
  return !trimmed.includes("\n") && LOCAL_COMMAND.test(trimmed);
}

/** Project a normalized setting value into the two words the reader needs.
 * Diagnostics deliberately fail this predicate and retain the generic command
 * treatment, so compact presentation never conceals a rejected command. */
export function localSetting(
  command: string | null,
  output: string | null,
): LocalSetting | null {
  if (command === null || output === null) return null;
  const value = output.trim();
  if (value === "" || value.includes("\n") || value.length > 96) return null;

  const name = command.trim().split(/\s+/, 1)[0]?.toLowerCase();
  if (name === "/effort") {
    return /^[a-z][\w-]*$/i.test(value)
      ? { kind: "effort", label: "Effort", value }
      : null;
  }
  if (name === "/model") {
    return /^(?:set|invalid|unknown|error|failed|usage)\b/i.test(value)
      ? null
      : { kind: "model", label: "Model", value };
  }
  return null;
}

/* ── segments ────────────────────────────────────────────────────────────── */

export type Segment =
  | {
      readonly kind: "prose";
      readonly key: string;
      readonly row: TimelineEntry;
      readonly at: number | null;
      readonly text: string | null;
      readonly attachments: readonly AttachmentFact[];
      readonly citations: readonly CitationRef[];
      readonly citationLabels: readonly string[];
    }
  | {
      readonly kind: "thinking";
      readonly key: string;
      readonly row: TimelineEntry;
      readonly at: number | null;
      readonly text: string;
      /** From adjacent stamps — what `more · reasoning, 12s` states. */
      readonly spanMs: number | null;
    }
  | {
      readonly kind: "ledger";
      readonly key: string;
      readonly at: number | null;
      readonly rows: readonly TimelineEntry[];
      readonly lines: readonly LedgerLine[];
      readonly summary: RunSummary;
    }
  | {
      readonly kind: "marker";
      readonly key: string;
      readonly row: TimelineEntry;
      readonly at: number | null;
      readonly notice: Notice;
      readonly text: string;
      /** A runtime error takes red TEXT — still a line, never a filled alarm. */
      readonly tone: "plain" | "failed";
    }
  | {
      readonly kind: "summary";
      readonly key: string;
      readonly row: TimelineEntry;
      readonly at: number | null;
      readonly purpose: SummaryPurpose;
      readonly text: string;
    }
  | {
      readonly kind: "collaboration";
      readonly key: string;
      readonly row: TimelineEntry;
      readonly at: number | null;
      readonly event: CollaborationEvent;
    };

/* ── the head ────────────────────────────────────────────────────────────── */

export interface ResponseHead {
  /** The model the reply opened with. A mid-reply change is a `model_changed`
   * marker in the segments, never a second head (§05 draws exactly that). */
  readonly model: string | null;
  readonly effort: string | null;
  /** Reported tokens across the reply's response groups. Null means the agent
   * never reported any, which must never be coerced to zero. */
  readonly tokens: number | null;
  readonly startedAt: number | null;
  readonly endedAt: number | null;
  readonly spanMs: number | null;
}

export interface Turn {
  /** Stable across renders: the prompt's `timeline_id`, else the first row's. */
  readonly key: string;
  readonly prompt: Prompt | null;
  /** One head per reply (T-4). Null when nothing agent-authored answered. */
  readonly head: ResponseHead | null;
  readonly segments: readonly Segment[];
  /** Where the eyebrow belongs: the index of the first agent-authored segment,
   * so a leading marker is not drawn under a head it does not belong to. */
  readonly headIndex: number;
  /** Every row in the turn, in order — anchors, focus landing, watermark. */
  readonly rows: readonly TimelineEntry[];
  readonly at: number | null;
  /** §07: this successful human setting command joins the previous setting
   * command's compact group. Conversational and parent-agent prompts are
   * always standalone turns. */
  readonly clusteredWithPrevious: boolean;
}

export interface DocumentOptions {
  /** Paths draw relative to the session's own cwd, then home-relative. */
  readonly context?: LedgerContext;
  /** Team child rows interleaved into their parent's shared chronology. Their
   * native parent-invocation group must not masquerade as a human turn here. */
  readonly sharedCollaborationSessionIds?: ReadonlySet<string>;
}

function opensTurn(row: TimelineEntry, options: DocumentOptions): boolean {
  if (row.body.type === "user") return true;
  if (
    row.body.type === "collaboration" &&
    options.sharedCollaborationSessionIds?.has(row.session_id) === true
  ) {
    return false;
  }
  return row.group.kind === "human_turn" || row.group.kind === "parent_invocation";
}

function isAgentAuthored(row: TimelineEntry): boolean {
  switch (row.body.type) {
    case "agent":
    case "thinking":
    case "tool_use":
    case "summary":
      return true;
    case "collaboration":
    case "user":
    case "notice":
      return false;
  }
}

/** Per-response snapshots, counted once each: `input + output + thinking`. */
function reportedTokens(rows: readonly TimelineEntry[]): number | null {
  const seen = new Set<string>();
  let total = 0;
  let reported = false;
  for (const row of rows) {
    const id = row.response?.id ?? row.timeline_id;
    if (seen.has(id)) continue;
    seen.add(id);
    const parts = [row.usage.input, row.usage.output, row.usage.thinking];
    if (parts.every((value) => value === null)) continue;
    reported = true;
    total += parts.reduce<number>((sum, value) => sum + (value ?? 0), 0);
  }
  return reported ? total : null;
}

function buildPrompt(
  row: TimelineEntry,
  outputRow: TimelineEntry | null,
): Prompt {
  const body = row.body;
  const collaboration = body.type === "collaboration" ? body.event : null;
  const rawText =
    body.type === "user" && body.text !== null
      ? canonicalText(body.text)
      : collaboration?.body !== null && collaboration?.body !== undefined
        ? canonicalText(collaboration.body)
        : (collaboration?.summary ?? null);
  const refs = body.type === "user" ? body.attachments : [];
  const { text, attachments } = readAttachments(rawText, refs);
  const isCommand = body.type === "user" && looksLocalCommand(text);
  const output =
    outputRow !== null && outputRow.body.type === "notice"
      ? canonicalText(
          outputRow.body.notice.type === "command_output"
            ? outputRow.body.notice.output
            : { type: "inline", text: "" },
        )
      : null;
  const command = isCommand && text !== null ? text.trim() : null;
  const commandOutput = output === null || output === "" ? null : output;
  return {
    key: row.timeline_id,
    row,
    rows: outputRow === null ? [row] : [row, outputRow],
    at: row.at,
    text,
    attachments,
    isCommand,
    command,
    commandOutput,
    commandOutputRow: outputRow,
    setting: localSetting(command, commandOutput),
    fromParent: row.group.kind === "parent_invocation",
    machine:
      collaboration === null
        ? null
        : {
            label:
              collaboration.kind === "assignment"
                ? "Assignment"
                : collaboration.channel === "coordinator"
                  ? "Coordinator follow-up"
                  : collaboration.channel === "peer"
                    ? "Peer message"
                    : "Team message",
            sender: collaboration.sender,
          },
  };
}

function buildSegments(
  rows: readonly TimelineEntry[],
  nextAtOf: (row: TimelineEntry) => number | null,
  context: LedgerContext,
): Segment[] {
  const segments: Segment[] = [];
  let run: TimelineEntry[] = [];
  const flush = () => {
    if (run.length === 0) return;
    const lines = ledgerLines(run, context);
    const stamps = run.map((row) => row.at).filter((value): value is number => value !== null);
    segments.push({
      kind: "ledger",
      key: run[0].timeline_id,
      at: stamps.length === 0 ? null : Math.min(...stamps),
      rows: run,
      lines,
      summary: runSummary(lines),
    });
    run = [];
  };

  for (const row of rows) {
    if (row.body.type === "tool_use") {
      run.push(row);
      continue;
    }
    flush();
    switch (row.body.type) {
      case "agent": {
        const text = row.body.text === null ? null : canonicalText(row.body.text);
        const { text: prose, attachments } = readAttachments(text, row.body.attachments);
        if (prose === null && attachments.length === 0 && row.body.citations.length === 0) break;
        segments.push({
          kind: "prose",
          key: row.timeline_id,
          row,
          at: row.at,
          text: prose,
          attachments,
          citations: row.body.citations,
          citationLabels: row.body.citations.map(citationLabel),
        });
        break;
      }
      case "thinking": {
        const text = canonicalText(row.body.text);
        if (text.trim() === "") break;
        const next = nextAtOf(row);
        segments.push({
          kind: "thinking",
          key: row.timeline_id,
          row,
          at: row.at,
          text,
          spanMs: row.at !== null && next !== null && next > row.at ? next - row.at : null,
        });
        break;
      }
      case "notice": {
        const notice = row.body.notice;
        segments.push({
          kind: "marker",
          key: row.timeline_id,
          row,
          at: row.at,
          notice,
          text: noticeText(notice),
          tone: notice.type === "runtime_error" ? "failed" : "plain",
        });
        break;
      }
      case "summary": {
        const text = canonicalText(row.body.text);
        if (text.trim() === "") break;
        segments.push({
          kind: "summary",
          key: row.timeline_id,
          row,
          at: row.at,
          purpose: row.body.purpose,
          text,
        });
        break;
      }
      case "collaboration":
        segments.push({
          kind: "collaboration",
          key: row.timeline_id,
          row,
          at: row.at,
          event: row.body.event,
        });
        break;
      case "user":
        // Unreachable: a user row always opens its own turn.
        break;
    }
  }
  flush();
  return segments;
}

/**
 * Fold an ordered timeline into turns. Pure: no clock, no IO. The rows behind
 * every prompt, segment and ledger line stay reachable, so the anchors, the
 * focus landing and the live watermark all keep working off `timeline_id` and
 * `ordinal` exactly as they do today.
 */
export function documentTurns(
  rows: readonly TimelineEntry[],
  options: DocumentOptions = {},
): Turn[] {
  const context = options.context ?? {};

  // The next stamped row, for a reasoning segment's span.
  const nextAt = new Map<string, number | null>();
  for (let index = 0; index < rows.length; index += 1) {
    let found: number | null = null;
    for (let ahead = index + 1; ahead < rows.length; ahead += 1) {
      if (rows[ahead].at !== null) {
        found = rows[ahead].at;
        break;
      }
    }
    nextAt.set(rows[index].timeline_id, found);
  }
  const nextAtOf = (row: TimelineEntry) => nextAt.get(row.timeline_id) ?? null;

  // Group the rows into turns, absorbing a command prompt's `command_output`
  // notice as we go — the pair is one object, decided by adjacency and nothing
  // else, so the notice never also draws as a marker.
  const groups: {
    prompt: TimelineEntry | null;
    output: TimelineEntry | null;
    body: TimelineEntry[];
  }[] = [];
  for (const row of rows) {
    if (opensTurn(row, options)) {
      groups.push({ prompt: row, output: null, body: [] });
      continue;
    }
    const current = groups.at(-1);
    if (current === undefined) {
      groups.push({ prompt: null, output: null, body: [row] });
      continue;
    }
    const promptRow = current.prompt;
    if (
      promptRow !== null &&
      current.output === null &&
      current.body.length === 0 &&
      row.body.type === "notice" &&
      row.body.notice.type === "command_output" &&
      looksLocalCommand(
        promptRow.body.type === "user" && promptRow.body.text !== null
          ? canonicalText(promptRow.body.text)
          : null,
      )
    ) {
      current.output = row;
      continue;
    }
    current.body.push(row);
  }

  const turns: Turn[] = [];
  let previousPromptAt: number | null = null;
  let previousHadReply = true;
  let previousWasHumanSetting = false;

  for (const group of groups) {
    const promptRow = group.prompt;
    const outputRow = group.output;
    const body = group.body;
    const prompt = promptRow === null ? null : buildPrompt(promptRow, outputRow);
    const segments = buildSegments(body, nextAtOf, context);
    const agentRows = body.filter(isAgentAuthored);
    const head: ResponseHead | null =
      agentRows.length === 0
        ? null
        : {
            model: agentRows.find((row) => row.model !== null)?.model ?? null,
            effort: agentRows.find((row) => row.effort !== null)?.effort ?? null,
            tokens: reportedTokens(agentRows),
            startedAt: firstStamp(body),
            endedAt: lastStamp(body),
            spanMs: stampSpan(body),
          };
    const headIndex = Math.max(
      0,
      segments.findIndex((segment) => segment.kind !== "marker"),
    );

    const at = promptRow?.at ?? firstStamp(body);
    const clusteredWithPrevious =
      prompt !== null &&
      prompt.setting !== null &&
      !prompt.fromParent &&
      previousWasHumanSetting &&
      !previousHadReply &&
      withinPromptCluster(previousPromptAt, prompt.at);

    const allRows = [
      ...(prompt === null ? [] : prompt.rows),
      ...body,
    ];
    turns.push({
      key: prompt?.key ?? allRows[0]?.timeline_id ?? `turn-${turns.length}`,
      prompt,
      head,
      segments,
      headIndex,
      rows: allRows,
      at,
      clusteredWithPrevious,
    });

    if (prompt !== null) {
      previousPromptAt = prompt.at;
      previousHadReply = agentRows.length > 0;
      previousWasHumanSetting = prompt.setting !== null && !prompt.fromParent;
    }
  }

  return turns;
}

function firstStamp(rows: readonly TimelineEntry[]): number | null {
  for (const row of rows) if (row.at !== null) return row.at;
  return null;
}

function lastStamp(rows: readonly TimelineEntry[]): number | null {
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    const at = rows[index].at;
    if (at !== null) return at;
  }
  return null;
}
