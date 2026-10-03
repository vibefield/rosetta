import assert from "node:assert/strict";
import { test } from "node:test";
import { createRoomStorage, DEFAULT_STORAGE_PREFIX } from "../renderer/src/lib/room-storage.ts";
import { createTintDealer, tintOf } from "../renderer/src/lib/tint.ts";
import { RECENT_QUERIES_KEY, rememberQuery, readRecentQueries } from "../renderer/src/lib/search-sheet.ts";

const memory = () => {
  const map = new Map();
  return { map, getItem: (key) => map.get(key) ?? null, setItem: (key, value) => { map.set(key, value); } };
};

test("every name the room stores goes under the host's prefix", () => {
  const backend = memory();
  const storage = createRoomStorage("vf.history.", backend);
  storage.setItem("tintSeq.v1", "{}");
  assert.equal(storage.getItem("tintSeq.v1"), "{}");
  assert.deepEqual([...backend.map.keys()], ["vf.history.tintSeq.v1"]);
  rememberQuery(storage, "ports");
  assert.deepEqual(readRecentQueries(storage), ["ports"]);
  assert.equal(backend.map.has(`vf.history.${RECENT_QUERIES_KEY}`), true);
});

test("the default prefix keeps the names rosetta's app has always used", () => {
  const backend = memory();
  const storage = createRoomStorage(DEFAULT_STORAGE_PREFIX, backend);
  rememberQuery(storage, "q");
  createTintDealer(storage).projectSeq("p");
  assert.deepEqual([...backend.map.keys()].sort(), ["ah.search.recent", "ah.tintSeq.v1"]);
});

test("a store that refuses, or none, costs the memory and never the room", () => {
  const refusing = { getItem() { throw new Error("blocked"); }, setItem() { throw new Error("blocked"); } };
  for (const backend of [null, refusing]) {
    const storage = createRoomStorage("x.", backend);
    assert.equal(storage.getItem("anything"), null);
    assert.doesNotThrow(() => storage.setItem("anything", "1"));
    const dealer = createTintDealer(storage);
    assert.equal(dealer.projectSeq("a"), 0);
    assert.equal(dealer.projectSeq("b"), 1);
    assert.equal(dealer.projectSeq("a"), 0, "the deal holds for this run");
  }
});

test("a dealt tint is remembered by the next room over the same store", () => {
  const storage = createRoomStorage("ah.", memory());
  const first = createTintDealer(storage);
  first.projectSeq("alpha");
  first.projectSeq("beta");
  const second = createTintDealer(storage);
  assert.equal(second.projectSeq("beta"), 1);
  assert.equal(second.projectSeq("gamma"), 2);
  assert.equal(second.projectTint("beta"), tintOf(1));
});
