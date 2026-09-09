import assert from "node:assert/strict";
import { test } from "node:test";
import { ledgerLines } from "../renderer/src/lib/transcript/ledger.ts";
import { webAddress } from "../renderer/src/lib/transcript/web-address.ts";
import { line, url } from "./support/ledger-fixtures.mjs";

const search = (sources, total = sources?.length) => ({ type: "web_search", query: "webgpu", allowed_domains: [], blocked_domains: [], outcome: sources === null ? null : { sources, total_count: total } });
const foldLooks = (lines) => ledgerLines(lines.flatMap((line) => line.rows));

test("web addresses preserve the destination and use the exact canonical host for icons", () => {
  assert.deepEqual(webAddress("https://WWW.Example.ORG.:8443/a?q=b#c"), { href: "https://www.example.org.:8443/a?q=b#c", host: "www.example.org" });
  for (const value of ["javascript:alert(1)", "data:text/html,test", "file:///tmp/test", "/relative", "turn1search0", "https://user:secret@example.org", null]) assert.equal(webAddress(value), null);
});

test("missing search output stays distinct from a proven empty result", () => {
  const missing = line(search(null)), empty = line(search([]));
  assert.equal(missing.detail.kind, "sources");
  assert.equal(missing.detail.total, null);
  assert.equal(empty.detail.total, 0);
  assert.equal(empty.result, "0 sources");
});

test("folded searches preserve the reported total beyond retained source previews", () => {
  const [fold] = foldLooks([line(search([{ url: "https://one.org", title: "One" }], 5)), line(search([{ url: "https://two.org", title: "Two" }], 8))]);
  assert.equal(fold.result, "13 sources");
  assert.equal(fold.detail.total, 13);
  assert.equal(fold.detail.sources.length, 2);
  const [partial] = foldLooks([line(search([{ url: "https://one.org", title: "One" }], 5)), line(search(null))]);
  assert.equal(partial.detail.total, null);
  assert.notEqual(partial.result, "5 sources");
});

test("folded fetched pages keep every URL, title, and status", () => {
  const [fold] = foldLooks(["one", "two"].map(name => line({ type: "web_fetch", url: url(`https://${name}.org/old`), purpose: null, outcome: { final_url: `https://${name}.org/final`, title: name, http_status: 200 } })));
  assert.equal(fold.detail.kind, "fetches");
  assert.deepEqual(fold.detail.fetches.map(fetch => fetch.url), ["https://one.org/final", "https://two.org/final"]);
  assert.deepEqual(fold.detail.fetches.map(fetch => fetch.title), ["one", "two"]);
});

test("fetch details retain a usable scheme, redirects, and an HTTP failure", () => {
  const noResult = line({ type: "web_fetch", url: url("https://example.org/a"), purpose: null, outcome: null });
  assert.equal(noResult.detail.fetch.url, "https://example.org/a");
  const failed = line({ type: "web_fetch", url: url("https://example.org/old"), purpose: null, outcome: { final_url: "https://www.example.org/new", title: null, http_status: 404 } }, { status: "failed", failure: { kind: "unknown", summary: null, diagnostic: "Not Found" } });
  assert.equal(failed.result, "HTTP 404");
  assert.equal(failed.detail.fetch.host, "www.example.org");
  assert.equal(failed.diagnostic, "Not Found");
});

test("find-in-page retains the searched pattern alongside its page address", () => {
  const found = line({
    type: "web_navigate",
    operation: "find_in_page",
    url: url("https://example.org/reference"),
    pattern: "vertex_index",
    outcome: null,
  });
  assert.equal(found.object.map(part => part.text).join(""), "vertex_index in example.org/reference");
  assert.equal(found.detail.fetch.url, "https://example.org/reference");
});
