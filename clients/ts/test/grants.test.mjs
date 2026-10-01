// The grant codec against the shared vectors (crates/server/tests/fixtures/
// grants.json), which a stdlib-Python generator wrote independently of both
// this module and crates/server/src/grant.rs.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { base64UrlDecode, base64UrlEncode } from "../base64url.ts";
import {
  MIN_GRANT_KEY_BYTES,
  canonicalPayload,
  canonicalize,
  grantAuthorization,
  mintGrant,
  parseGrantHeader,
  verifyGrant,
} from "../grants.ts";

const fixture = JSON.parse(
  readFileSync(new URL("../../../crates/server/tests/fixtures/grants.json", import.meta.url), "utf8"),
);
const key = Uint8Array.from(Buffer.from(fixture.key_hex, "hex"));

test("every valid vector is reproduced byte for byte", () => {
  assert.ok(fixture.valid.length > 0);
  for (const vector of fixture.valid) {
    assert.equal(canonicalPayload(vector.claims), vector.payload, vector.name);
    assert.equal(mintGrant(key, vector.claims), vector.grant, vector.name);
    assert.deepEqual(
      verifyGrant(key, vector.grant, fixture.now_ms),
      { ok: true, claims: vector.claims },
      vector.name,
    );
    assert.deepEqual(
      parseGrantHeader(grantAuthorization(vector.grant)),
      { grant: vector.grant, claims: vector.claims },
      vector.name,
    );
  }
});

test("every invalid vector is refused for its reason", () => {
  assert.ok(fixture.invalid.length > 0);
  for (const vector of fixture.invalid) {
    assert.deepEqual(
      verifyGrant(key, vector.grant, fixture.now_ms),
      { ok: false, reason: vector.reason },
      vector.name,
    );
  }
});

test("canonicalize is RFC 8785: code-unit member order, JSON.stringify scalars", () => {
  assert.equal(
    canonicalize({ b: [1, "x", null, true], a: { d: 1.5, c: -0 } }),
    '{"a":{"c":0,"d":1.5},"b":[1,"x",null,true]}',
  );
  // UTF-16 code units, not code points: U+1F600 (D83D DE00) sorts before U+FF21.
  assert.equal(canonicalize({ "Ａ": 1, "\u{1F600}": 2 }), '{"\u{1F600}":2,"Ａ":1}');
  assert.throws(() => canonicalize({ n: Number.POSITIVE_INFINITY }), TypeError);
  assert.throws(() => canonicalize({ s: "\ud800" }), TypeError);
});

test("minting refuses a short key and claims a daemon would call malformed", () => {
  const claims = fixture.valid[0].claims;
  assert.throws(() => mintGrant(key.slice(0, MIN_GRANT_KEY_BYTES - 1), claims), RangeError);
  for (const bad of [
    { ...claims, v: 2 },
    { ...claims, sid: "" },
    { ...claims, cid: "" },
    { ...claims, exp: 1.5 },
    { ...claims, exp: 2 ** 53 },
    { ...claims, scope: [1] },
  ]) {
    assert.throws(() => mintGrant(key, bad), TypeError, JSON.stringify(bad));
  }
});

test("parseGrantHeader reads a grant without verifying it", () => {
  const { claims } = fixture.valid[0];
  const foreign = mintGrant(new Uint8Array(32).fill(1), claims);
  assert.deepEqual(parseGrantHeader(`rosetta-grant   ${foreign}`), { grant: foreign, claims });
  assert.deepEqual(verifyGrant(key, foreign, fixture.now_ms), { ok: false, reason: "bad-signature" });
  assert.equal(parseGrantHeader("Bearer abc"), null);
  assert.equal(parseGrantHeader("not.a-grant"), null);
  assert.equal(parseGrantHeader(fixture.invalid.find((v) => v.name === "no MAC").grant), null);
});

test("base64url is strict: one spelling per byte string", () => {
  for (const length of [0, 1, 2, 3, 4, 5, 31, 32, 33]) {
    const bytes = Uint8Array.from({ length }, (_, index) => (index * 37 + 11) & 255);
    const text = base64UrlEncode(bytes);
    assert.equal(text, Buffer.from(bytes).toString("base64url"), `length ${length}`);
    assert.deepEqual(base64UrlDecode(text), bytes, `length ${length}`);
  }
  for (const bad of ["A", "AB==", "AB+/", "AB", "A.B"]) {
    assert.equal(base64UrlDecode(bad), null, bad);
  }
  assert.deepEqual(base64UrlDecode("AA"), new Uint8Array([0]));
});
