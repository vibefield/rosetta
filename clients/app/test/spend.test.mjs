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
} from "../renderer/src/lib/spend.ts";

const row = (cost, billed, unpriced) => ({
  cost_micro_usd: cost,
  billed_micro_usd: billed,
  unpriced_tokens: unpriced,
});

describe("spend", () => {
  it("adds nullable amounts without inventing zeroes", () => {
    const total = sumSpend([row(null, null, 40), row(1_500_000, null, 0), row(null, 508_000_000, 10)]);
    assert.deepEqual(total, {
      costMicroUsd: 1_500_000,
      billedMicroUsd: 508_000_000,
      unpricedTokens: 50,
    });
    assert.deepEqual(addSpend(NO_SPEND, NO_SPEND), NO_SPEND);
  });

  it("names coverage from the columns alone", () => {
    assert.equal(coverage(spendOf(row(10, null, 0))), "complete");
    assert.equal(coverage(spendOf(row(10, null, 5))), "partial");
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

  it("describes billed amounts beside, never inside, the list price", () => {
    const text = spendDescription(spendOf(row(null, 508_000_000, 2_000)), (n) => `${n}`);
    assert.match(text, /2000 tokens with no list price/);
    assert.match(text, /\$508\.00 billed by the provider/);
    assert.doesNotMatch(text, /at API list prices/);
  });
});
