// T-4 / §03 B — one head per response, never per message.
//
// Four CLAUDE-OPUS-5 eyebrows on one screen, each over one sentence, is the
// shipped column's loudest mistake. The eyebrow appears ONCE per reply: the
// agent's monochrome mark and name, the model and effort as a quiet caption,
// and at the right the span and the reported tokens.
//
// Tokens are the agent's own figure and are never coerced to zero — an agent
// that reported none says nothing here rather than claiming a total. The cost
// beside them is the list-price equivalent (§4.1.3), marked `+` when part of
// the reply has no list price.
import { AgentMark, agentLabel } from "../../lib/agents";
import { tokenText } from "../../lib/format";
import { usdExact, usdText } from "../../lib/spend";
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
  // A reply with usage but no priced part still says so (`n/p`), never
  // nothing: an unpriced model must not read as free.
  const cost = head.cost;
  const costText = cost === null
    ? null
    : cost.microUsd === null
      ? "n/p"
      : `${usdText(cost.microUsd)}${cost.partial ? "+" : ""}`;
  const costTitle = cost === null
    ? undefined
    : cost.microUsd === null
      ? "No list price for this reply's model"
      : `${usdExact(cost.microUsd)} at API list prices${cost.partial ? " · a lower bound: part of this reply has no list price or no recorded output" : ""}`;
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
          {head.tokens !== null && costText !== null && " · "}
          {costText !== null && <b title={costTitle}>{costText}</b>}
        </span>
      )}
    </div>
  );
}
