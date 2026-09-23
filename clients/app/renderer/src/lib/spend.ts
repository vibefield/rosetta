// List-price spend (docs/ARCHITECTURE.md §4.1.3): what the recorded usage
// would cost at the model creator's API list price. One basis for every
// agent; a provider-billed amount is a separate, secondary figure; usage the
// price catalog cannot price is counted and shown, never read as $0.

/** The wire's spend columns, flattened beside every aggregate `tokens`. */
export interface SpendFields {
  readonly cost_micro_usd: number | null;
  readonly billed_micro_usd: number | null;
  readonly unpriced_tokens: number;
}

export interface Spend {
  /** List-price micro-USD of the priced usage; null when nothing was priced. */
  readonly costMicroUsd: number | null;
  /** Provider-billed micro-USD where the agent records one; null otherwise. */
  readonly billedMicroUsd: number | null;
  /** Tokens the price catalog could not price. */
  readonly unpricedTokens: number;
}

export const NO_SPEND: Spend = { costMicroUsd: null, billedMicroUsd: null, unpricedTokens: 0 };

export function spendOf(fields: SpendFields): Spend {
  return {
    costMicroUsd: fields.cost_micro_usd,
    billedMicroUsd: fields.billed_micro_usd,
    unpricedTokens: fields.unpriced_tokens,
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
  };
}

export function sumSpend(rows: Iterable<SpendFields>): Spend {
  let total = NO_SPEND;
  for (const row of rows) total = addSpend(total, spendOf(row));
  return total;
}

/** `complete`: every token priced. `partial`: some unpriced, so the figure
 * is a lower bound. `unavailable`: tokens, none priced. `none`: no usage. */
export type Coverage = "complete" | "partial" | "unavailable" | "none";

export function coverage(spend: Spend): Coverage {
  if (spend.costMicroUsd === null) return spend.unpricedTokens > 0 ? "unavailable" : "none";
  return spend.unpricedTokens > 0 ? "partial" : "complete";
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
    parts.push(`${usdExact(spend.costMicroUsd)} at API list prices`);
  }
  if (spend.unpricedTokens > 0) {
    parts.push(
      `${tokenText(spend.unpricedTokens)} tokens ${spend.costMicroUsd === null ? "" : "more "}with no list price`,
    );
  }
  if (spend.billedMicroUsd !== null) {
    parts.push(`${usdExact(spend.billedMicroUsd)} billed by the provider`);
  }
  return parts.length === 0 ? "No recorded usage" : parts.join(" · ");
}
