// T-4 / §03 B — one head per response, never per message.
//
// Four CLAUDE-OPUS-5 eyebrows on one screen, each over one sentence, is the
// shipped column's loudest mistake. The eyebrow appears ONCE per reply: the
// agent's monochrome mark and name, the model and effort as a quiet caption,
// and at the right the span and the reported tokens.
//
// Tokens are the agent's own figure and are never coerced to zero — an agent
// that reported none says nothing here rather than claiming a total.
import { AgentMark, agentLabel } from "../../lib/agents";
import { tokenText } from "../../lib/format";
import type { ResponseHead as HeadModel } from "../../lib/transcript";
import { spanText } from "../../lib/transcript";

export interface ResponseHeadProps {
  readonly head: HeadModel;
  /** The session's runtime, e.g. `claude-code` — the mark and the name. */
  readonly runtimeKind: string;
  /** A live reply whose end has not landed reads `1m 12s → …` (§05). */
  readonly open: boolean;
}

export function ResponseHead({ head, runtimeKind, open }: ResponseHeadProps) {
  const caption = [head.model, head.effort === null ? null : `${head.effort} effort`]
    .filter((piece): piece is string => piece !== null && piece !== "")
    .join(" · ");
  const span = head.spanMs === null ? null : spanText(head.spanMs);
  return (
    <div className="tr-head">
      <span className="mark">
        <AgentMark kind={runtimeKind} />
      </span>
      {agentLabel(runtimeKind)}
      {caption !== "" && <span className="model">{caption}</span>}
      {(span !== null || head.tokens !== null) && (
        <span className="facts">
          {span !== null && (open ? `${span} → …` : span)}
          {span !== null && head.tokens !== null && " · "}
          {head.tokens !== null && <b>{tokenText(head.tokens)}</b>}
        </span>
      )}
    </div>
  );
}
