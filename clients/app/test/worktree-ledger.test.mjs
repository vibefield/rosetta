import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { ledgerOf, settleOrder } from "../../room/src/components/worktree/worktree-ledger.ts";

const GB = 1024 ** 3;
const scratch = "/private/tmp/claude-501/-Users-a-Projects-field/00000000-0000-4000-8000-000000000000/scratchpad";
const listed = [
  { path: "/Users/a/field", branch: "main", head: "1a2b3c49a1b2c3d4e5f60718293a4b5c6d7e8f90", gone: false, sizeBytes: 2 * GB },
  { path: "/Users/a/stack", branch: "co/parts-186-187-stack-v2", head: "aaaaaaa1", gone: false, sizeBytes: 6 * GB },
  { path: `${scratch}/wt-10bfix`, branch: null, head: "0e4919fff0000000000000000000000000000000", gone: false, sizeBytes: null },
  { path: `${scratch}/wt-11a-green/`, branch: null, head: "8436ffab40000000000000000000000000000000", gone: false, sizeBytes: 1 * GB },
  { path: `${scratch}/wt-09-probe`, branch: null, head: "22fdbadc00000000000000000000000000000000", gone: true },
];

describe("the worktree ledger", () => {
  it("stands measured rows heaviest first, then the unmeasured, then the gone", () => {
    const ledger = ledgerOf(listed, "/Users/a/field");
    assert.deepEqual(ledger.rows.map((row) => row.name), [
      "co/parts-186-187-stack-v2", "main", "wt-11a-green", "wt-10bfix", "wt-09-probe",
    ]);
    assert.deepEqual(ledger.rows.map((row) => row.share), [1, 2 / 6, 1 / 6, null, null]);
    assert.deepEqual(ledger.rows.map((row) => row.open), [false, true, false, false, false]);
    assert.equal(ledger.totalBytes, 9 * GB);
    assert.equal(ledger.partial, true, "one row is still being measured, so the sum is a floor");
  });

  it("names a checkout with no branch by its folder and its commit, never its path", () => {
    const rows = ledgerOf(listed, null).rows;
    const named = Object.fromEntries(rows.map((row) => [row.name, row.commit]));
    assert.deepEqual(named, {
      "co/parts-186-187-stack-v2": null,
      main: null,
      "wt-11a-green": "8436ffa",
      "wt-10bfix": "0e4919f",
      "wt-09-probe": "22fdbad",
    });
  });

  it("says nothing of size for a folder that is gone, whatever a host sends", () => {
    const [row] = ledgerOf([{ path: "/gone/wt", branch: "old", gone: true, sizeBytes: 5 * GB }], null).rows;
    assert.equal(row.gone, true);
    assert.equal(row.sizeBytes, undefined);
    assert.equal(row.share, null);
  });

  it("is git's own order, with no figures, on a host that measures nothing", () => {
    const plain = [
      { path: "/r/main", branch: "main" },
      { path: "/r/b", branch: "b" },
      { path: "/r/a", branch: "a" },
    ];
    const ledger = ledgerOf(plain, "/r/b");
    assert.deepEqual(ledger.rows.map((row) => row.name), ["main", "b", "a"]);
    assert.deepEqual(ledger.rows.map((row) => row.sizeBytes), [undefined, undefined, undefined]);
    assert.equal(ledger.totalBytes, null);
    assert.equal(ledger.partial, false);
  });

  it("takes each row's figure from what the list knows now, not only from what git facts carried", () => {
    const answers = new Map([[listed[2].path, 12 * GB], [listed[0].path, null]]);
    const ledger = ledgerOf(listed, null, (w) => (answers.has(w.path) ? answers.get(w.path) : w.sizeBytes));
    assert.deepEqual(ledger.rows.map((row) => [row.name, row.sizeBytes === null ? "owed" : row.sizeBytes]), [
      ["wt-10bfix", 12 * GB], ["co/parts-186-187-stack-v2", 6 * GB], ["wt-11a-green", 1 * GB], ["main", "owed"], ["wt-09-probe", undefined],
    ]);
    assert.equal(ledger.totalBytes, 19 * GB);
    assert.equal(ledger.partial, true);
  });

  it("draws the ledger's order at rest, and keeps every row's place while the list is being read", () => {
    const ranked = ["heavy", "middle", "light", "owed"];
    assert.deepEqual(settleOrder([], ranked, false), ranked, "a first drawing is the ledger's order");
    assert.deepEqual(settleOrder(["owed", "light", "middle", "heavy"], ranked, true), ranked);
    assert.deepEqual(settleOrder(["owed", "light", "middle", "heavy"], ranked, false), ["owed", "light", "middle", "heavy"],
      "a figure arriving moves nothing under the reader");
    assert.deepEqual(settleOrder(["owed", "gone-since", "light"], ranked, false), ["owed", "light", "heavy", "middle"],
      "a row no longer listed goes, and new rows follow in the ledger's order");
  });

  it("keeps git's order between equal figures and draws an empty folder's bar at nothing", () => {
    const ledger = ledgerOf([
      { path: "/r/one", branch: "one", sizeBytes: 0 },
      { path: "/r/two", branch: "two", sizeBytes: 0 },
    ], null);
    assert.deepEqual(ledger.rows.map((row) => [row.name, row.share]), [["one", 0], ["two", 0]]);
    assert.equal(ledger.totalBytes, 0);
  });
});
