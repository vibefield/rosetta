import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { fmtCompactNumber, fmtTokens, tokenText } from "../../room/src/lib/format.ts";

describe("compact summary numbers", () => {
  it("uses the same scale and precision for counts and tokens", () => {
    for (const [value, n, u] of [
      [0, "0", ""], [128, "128", ""], [999, "999", ""],
      [1_701, "1.70", "K"], [742_215, "742", "K"], [209_628, "210", "K"],
      [42_300_000_000, "42.3", "B"], [112_000_000_000, "112", "B"],
      [1_234_000_000_000, "1.23", "T"],
    ]) {
      assert.deepEqual(fmtCompactNumber(value), { n, u });
      assert.deepEqual(fmtTokens(value), { n, u });
      assert.equal(tokenText(value), n + u);
    }
  });

  it("keeps three significant digits when rounding crosses a digit or unit boundary", () => {
    for (const [value, n, u] of [
      [999.5, "1.00", "K"], [9_995, "10.0", "K"], [9_999, "10.0", "K"],
      [99_950, "100", "K"], [99_999, "100", "K"],
      [999_499, "999", "K"], [999_500, "1.00", "M"],
      [999_500_000, "1.00", "B"], [999_500_000_000, "1.00", "T"],
    ]) assert.deepEqual(fmtCompactNumber(value), { n, u });
  });
});
