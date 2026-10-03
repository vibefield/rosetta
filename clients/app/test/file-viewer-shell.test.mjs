import { strict as assert } from "node:assert";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

const roomCss = readFileSync(new URL("../renderer/src/styles/room.css", import.meta.url), "utf8");
// The window's drag region is the app's, not the room's: the room only marks
// its root while one of its dialogs is open (test/room-host covers the mark).
const appCss = readFileSync(new URL("../renderer/src/styles/app.css", import.meta.url), "utf8");

describe("file viewer desktop shell", () => {
  it("reclaims pointer input from the native title-bar drag region", () => {
    assert.match(appCss, /\n\.ah-searchbar\s*{[^}]*-webkit-app-region:\s*drag;/);
    assert.match(
      appCss,
      /\.ah-frame\[data-ah-overlay\] \.ah-searchbar\s*{[^}]*-webkit-app-region:\s*no-drag;/,
    );
    assert.match(
      roomCss,
      /\.ah-fileview\s*{[^}]*-webkit-app-region:\s*no-drag;/s,
    );
  });
});
