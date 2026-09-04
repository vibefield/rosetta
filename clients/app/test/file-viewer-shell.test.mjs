import { strict as assert } from "node:assert";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

const roomCss = readFileSync(new URL("../renderer/src/styles/room.css", import.meta.url), "utf8");

describe("file viewer desktop shell", () => {
  it("reclaims pointer input from the native title-bar drag region", () => {
    assert.match(
      roomCss,
      /body:has\(\[data-ah-fileview\]\) \.ah-searchbar\s*{[^}]*-webkit-app-region:\s*no-drag;/,
    );
    assert.match(
      roomCss,
      /\.ah-fileview\s*{[^}]*-webkit-app-region:\s*no-drag;/s,
    );
  });
});
