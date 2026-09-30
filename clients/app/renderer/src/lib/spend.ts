// Estimated spend (docs/ARCHITECTURE.md §4.1.3): recorded requests at the
// model creator's API list price, supplemented by higher reported session
// totals. A provider-billed amount is a separate, secondary figure; usage the
// price catalog cannot price is counted and shown, never read as $0.

/** The wire's spend columns, flattened beside every aggregate `tokens`. */
export interface SpendFields {
  readonly cost_micro_usd: number | null;
  readonly billed_micro_usd: number | null;
  readonly unpriced_tokens: number;
  readonly unrecorded_output: number;
  readonly reported_cost_adjustment_micro_usd?: number;
}

export interface Spend {
  /** Estimated micro-USD, including reported adjustments; null when unpriced. */
  readonly costMicroUsd: number | null;
  /** Provider-billed micro-USD where the agent records one; null otherwise. */
  readonly billedMicroUsd: number | null;
  /** Tokens the price catalog could not price. */
  readonly unpricedTokens: number;
  /** Usage records whose prompt was recorded but whose output never was
   * (a streamed reply that never got its final usage record): the priced
   * figure is a lower bound. */
  readonly unrecordedOutput: number;
  /** Additional estimate recovered from cumulative agent totals. */
  readonly reportedCostAdjustmentMicroUsd: number;
}

export const NO_SPEND: Spend = {
  costMicroUsd: null,
  billedMicroUsd: null,
  unpricedTokens: 0,
  unrecordedOutput: 0,
  reportedCostAdjustmentMicroUsd: 0,
};

export function spendOf(fields: SpendFields): Spend {
  return {
    costMicroUsd: fields.cost_micro_usd,
    billedMicroUsd: fields.billed_micro_usd,
    unpricedTokens: fields.unpriced_tokens,
    unrecordedOutput: fields.unrecorded_output,
    reportedCostAdjustmentMicroUsd: fields.reported_cost_adjustment_micro_usd ?? 0,
  };
}

function addNullable(left: number | null, right: number | null): number | null {
  if (left === null) return right;
  if (right === null) return left;
  return left + right;
}

export function addSpend(left: Spend, right: Spend): Spend {
  return {
    costMicroUsd: addNullable(left.costMicroUsd, right.costMicroUsd),
    billedMicroUsd: addNullable(left.billedMicroUsd, right.billedMicroUsd),
    unpricedTokens: left.unpricedTokens + right.unpricedTokens,
    unrecordedOutput: left.unrecordedOutput + right.unrecordedOutput,
    reportedCostAdjustmentMicroUsd: left.reportedCostAdjustmentMicroUsd + right.reportedCostAdjustmentMicroUsd,
  };
}

export function sumSpend(rows: Iterable<SpendFields>): Spend {
  let total = NO_SPEND;
  for (const row of rows) total = addSpend(total, spendOf(row));
  return total;
}

/** `complete`: every token priced and every output recorded. `partial`: some
 * usage unpriced or some output unrecorded, so the figure is a lower bound.
 * `unavailable`: tokens, none priced. `none`: no usage. */
export type Coverage = "complete" | "partial" | "unavailable" | "none";

export function coverage(spend: Spend): Coverage {
  if (spend.costMicroUsd === null) return spend.unpricedTokens > 0 ? "unavailable" : "none";
  return spend.unpricedTokens > 0 || spend.unrecordedOutput > 0 ? "partial" : "complete";
}

/** The two-part figure (`$53.5` + `K`), in fmtTokens' shape. */
export function fmtUsd(microUsd: number): { n: string; u: string } {
  const usd = microUsd / 1e6;
  const scaled = (value: number) =>
    value >= 100 ? value.toFixed(0) : value >= 10 ? value.toFixed(1) : value.toFixed(2);
  if (usd >= 1e9) return { n: `$${scaled(usd / 1e9)}`, u: "B" };
  if (usd >= 1e6) return { n: `$${scaled(usd / 1e6)}`, u: "M" };
  if (usd >= 1e3) return { n: `$${scaled(usd / 1e3)}`, u: "K" };
  if (usd >= 100) return { n: `$${usd.toFixed(0)}`, u: "" };
  if (usd >= 0.01 || usd === 0) return { n: `$${usd.toFixed(2)}`, u: "" };
  return { n: "<$0.01", u: "" };
}

export function usdText(microUsd: number): string {
  const figure = fmtUsd(microUsd);
  return figure.n + figure.u;
}

/** A long form for titles and accessible labels: `$1,234.56`. */
export function usdExact(microUsd: number): string {
  return (microUsd / 1e6).toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: microUsd < 10_000 ? 4 : 2,
  });
}

/** The compact figure with its coverage marker: `$53.5K`, `$53.5K+` when some
 * usage is unpriced (a lower bound), `unpriced` when none was priced. */
export function spendText(spend: Spend): string | null {
  switch (coverage(spend)) {
    case "none":
      return null;
    case "unavailable":
      return "unpriced";
    case "partial":
      return `${usdText(spend.costMicroUsd ?? 0)}+`;
    case "complete":
      return usdText(spend.costMicroUsd ?? 0);
  }
}

/** One sentence for a title or accessible label. */
export function spendDescription(spend: Spend, tokenText: (tokens: number) => string): string {
  const parts: string[] = [];
  if (spend.costMicroUsd !== null) {
    if (spend.reportedCostAdjustmentMicroUsd > 0) {
      parts.push(`${usdExact(spend.costMicroUsd)} estimated cost`);
      parts.push(`${usdExact(spend.costMicroUsd - spend.reportedCostAdjustmentMicroUsd)} from recorded requests at API list prices`);
      parts.push(`${usdExact(spend.reportedCostAdjustmentMicroUsd)} added from agent-reported session totals`);
    } else {
      parts.push(`${usdExact(spend.costMicroUsd)} at API list prices`);
    }
  }
  if (spend.unpricedTokens > 0) {
    parts.push(
      `${tokenText(spend.unpricedTokens)} tokens ${spend.costMicroUsd === null ? "" : "more "}with no list price`,
    );
  }
  if (spend.unrecordedOutput > 0) {
    parts.push(
      `${spend.unrecordedOutput.toLocaleString()} ${spend.unrecordedOutput === 1 ? "reply" : "replies"} with unrecorded output`,
    );
  }
  if (spend.billedMicroUsd !== null) {
    parts.push(`${usdExact(spend.billedMicroUsd)} billed by the provider`);
  }
  return parts.length === 0 ? "No usage read from this agent's records" : parts.join(" · ");
}
