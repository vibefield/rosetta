// Issue #27 — the host titles the not-answering face. A host that never names
// its daemon to a person (VibeField: "History isn't answering") hands the room
// its own titles; with none, the room keeps its own words, so rosetta's app
// reads as before. The closed face needs a live subscription, so the Electron
// room-host check covers it; here the server render covers the face the room
// draws before the index has said anything.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { HistoryRoom } from "../../room/src/HistoryRoom.tsx";
import { TranscriptReader } from "../../room/src/TranscriptReader.tsx";
import { createDemoPorts } from "../../room/src/fixtures/index.ts";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const fromRoom = createRequire(path.join(ROOM, "package.json"));
const { createElement } = fromRoom("react");
const { renderToStaticMarkup } = fromRoom("react-dom/server");

const titleOf = (markup) => markup.match(/<div class="ah-empty"[^>]*>.*?<strong>(.*?)<\/strong>/s)?.[1];

test("the room's own title while it reaches the index, when the host gives none", () => {
  const markup = renderToStaticMarkup(createElement(HistoryRoom, { ...createDemoPorts(), storage: null }));
  assert.equal(titleOf(markup), "Connecting to rosettad");
});

test("the host's title while it reaches the index, and no daemon named", () => {
  const markup = renderToStaticMarkup(createElement(HistoryRoom, {
    ...createDemoPorts(),
    storage: null,
    connectingTitle: "Opening History",
    unavailableTitle: "History isn't answering",
    unavailable: "VibeField is restarting it.",
  }));
  assert.equal(titleOf(markup), "Opening History");
  assert.match(markup, /<p>VibeField is restarting it\.<\/p>/, "the host's words stay under its title");
  assert.doesNotMatch(markup, /rosettad/);
});

test("the reader's opening face is its own, whatever title the host gives for a closed index", () => {
  const at = { sessionId: "s0" };
  const plain = renderToStaticMarkup(createElement(TranscriptReader, { ...createDemoPorts(), storage: null, at }));
  const titled = renderToStaticMarkup(createElement(TranscriptReader, {
    ...createDemoPorts(), storage: null, at, unavailableTitle: "History isn't answering",
  }));
  assert.equal(titleOf(plain), "Opening the session");
  assert.equal(titleOf(titled), "Opening the session");
});
