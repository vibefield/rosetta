import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { fmtCompactNumber, fmtTokens, tildePath, tokenText } from "../../room/src/lib/format.ts";

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

describe("home-relative paths", () => {
  it("abbreviates the home folder and what is under it", () => {
    assert.equal(tildePath("/Users/a", "/Users/a"), "~");
    assert.equal(tildePath("/Users/a/x", "/Users/a"), "~/x");
    assert.equal(tildePath("/Users/a/Projects/vibe-field", "/Users/a"), "~/Projects/vibe-field");
  });

  it("stops at a folder boundary: a sibling that shares the home's spelling is not under it", () => {
    assert.equal(tildePath("/Users/ab/x", "/Users/a"), "/Users/ab/x");
    assert.equal(tildePath("/Users/ab", "/Users/a"), "/Users/ab");
    assert.equal(tildePath("/Users/a-old/x", "/Users/a"), "/Users/a-old/x");
  });

  it("reads a home given with a trailing slash as the same folder", () => {
    assert.equal(tildePath("/Users/a/x", "/Users/a/"), "~/x");
    assert.equal(tildePath("/Users/a", "/Users/a/"), "~");
  });

  it("leaves a path whole when there is no home, or the home is the root", () => {
    assert.equal(tildePath("/Users/a/x", null), "/Users/a/x");
    assert.equal(tildePath("/Users/a/x", ""), "/Users/a/x");
    assert.equal(tildePath("/etc/hosts", "/"), "/etc/hosts");
  });
});
