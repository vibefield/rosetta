// List-price spend (ARCHITECTURE §4.1.3): one basis, coverage always shown,
// unpriced usage never read as $0.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  NO_SPEND,
  addSpend,
  coverage,
  fmtUsd,
  spendDescription,
  spendOf,
  spendText,
  sumSpend,
  usdText,
} from "../../room/src/lib/spend.ts";

const row = (cost, billed, unpriced, unrecorded = 0) => ({
  cost_micro_usd: cost,
  billed_micro_usd: billed,
  unpriced_tokens: unpriced,
  unrecorded_output: unrecorded,
});

describe("spend", () => {
  it("adds nullable amounts without inventing zeroes", () => {
    const total = sumSpend([
      row(null, null, 40),
      row(1_500_000, null, 0, 2),
      row(null, 508_000_000, 10, 1),
    ]);
    assert.deepEqual(total, {
      costMicroUsd: 1_500_000,
      billedMicroUsd: 508_000_000,
      unpricedTokens: 50,
      unrecordedOutput: 3,
      reportedCostAdjustmentMicroUsd: 0,
    });
    assert.deepEqual(addSpend(NO_SPEND, NO_SPEND), NO_SPEND);
  });

  it("names coverage from the columns alone", () => {
    assert.equal(coverage(spendOf(row(10, null, 0))), "complete");
    assert.equal(coverage(spendOf(row(10, null, 5))), "partial");
    // A reply whose output was never recorded makes the figure a lower bound.
    assert.equal(coverage(spendOf(row(10, null, 0, 1))), "partial");
    assert.equal(spendText(spendOf(row(10_000_000, null, 0, 1))), "$10.00+");
    assert.match(spendDescription(spendOf(row(10, null, 0, 2)), (n) => `${n}`), /2 replies with unrecorded output/);
    assert.equal(coverage(spendOf(row(null, null, 5))), "unavailable");
    assert.equal(coverage(NO_SPEND), "none");
  });

  it("formats like the token figures, with a lower-bound marker", () => {
    assert.deepEqual(fmtUsd(53_512_340_000), { n: "$53.5", u: "K" });
    assert.deepEqual(fmtUsd(1_234_000_000_000), { n: "$1.23", u: "M" });
    assert.equal(usdText(420_000), "$0.42");
    assert.equal(usdText(3_000), "<$0.01");
    assert.equal(spendText(spendOf(row(53_512_340_000, null, 0))), "$53.5K");
    assert.equal(spendText(spendOf(row(53_512_340_000, null, 9))), "$53.5K+");
    assert.equal(spendText(spendOf(row(null, null, 9))), "unpriced");
    assert.equal(spendText(NO_SPEND), null);
  });

  it("uses the shared precision and carries rounded dollars into the next unit", () => {
    for (const [dollars, n, u] of [
      [999.5, "$1.00", "K"], [9_995, "$10.0", "K"], [9_999, "$10.0", "K"],
      [99_950, "$100", "K"], [99_999, "$100", "K"],
      [999_499, "$999", "K"], [999_500, "$1.00", "M"],
      [999_500_000, "$1.00", "B"], [999_500_000_000, "$1.00", "T"],
    ]) assert.deepEqual(fmtUsd(dollars * 1e6), { n, u });
    assert.equal(usdText(0), "$0.00");
    assert.equal(usdText(12_340_000), "$12.34");
  });

  it("describes billed amounts beside, never inside, the list price", () => {
    const text = spendDescription(spendOf(row(null, 508_000_000, 2_000)), (n) => `${n}`);
    assert.match(text, /2000 tokens with no list price/);
    assert.match(text, /\$508\.00 billed by the provider/);
    assert.doesNotMatch(text, /at API list prices/);
  });

  it("separates a reported estimate adjustment from list prices and billing", () => {
    const spend = spendOf({ ...row(193_134_526, null, 0, 2), reported_cost_adjustment_micro_usd: 25_053_951 });
    const text = spendDescription(spend, String);
    assert.match(text, /\$193\.13 estimated cost/);
    assert.match(text, /\$168\.08 from recorded requests at API list prices/);
    assert.match(text, /\$25\.05 added from agent-reported session totals/);
    assert.doesNotMatch(text, /billed by the provider/);
    assert.equal(addSpend(NO_SPEND, spend).reportedCostAdjustmentMicroUsd, 25_053_951);
  });
});
