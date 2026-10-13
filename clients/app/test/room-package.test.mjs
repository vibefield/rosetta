// @vibefield/rosetta-room keeps its promises: its exports exist, its entry loads, its
// imports stay inside it, and its source dials nothing and keeps nothing
// outside its prefix (README, "What the package never does").
import assert from "node:assert/strict";
import { readFileSync, readdirSync, existsSync, statSync } from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const ROOM = fileURLToPath(new URL("../../room/", import.meta.url));
const SRC = path.join(ROOM, "src");
const manifest = JSON.parse(readFileSync(path.join(ROOM, "package.json"), "utf8"));
const sources = (function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const full = path.join(dir, name);
    return statSync(full).isDirectory() ? walk(full) : /\.(ts|tsx)$/.test(name) ? [full] : [];
  });
})(SRC);
const DEPENDENCIES = new Set([...Object.keys(manifest.dependencies), ...Object.keys(manifest.peerDependencies)]);

test("every export the manifest names is there, and the css is a side effect", () => {
  assert.deepEqual(Object.keys(manifest.exports), [".", "./fixtures", "./styles/room.css"]);
  for (const entry of Object.values(manifest.exports)) {
    for (const target of typeof entry === "string" ? [entry] : Object.values(entry)) {
      assert.equal(existsSync(path.join(ROOM, target)), true, target);
    }
  }
  assert.deepEqual(manifest.peerDependencies, { react: ">=19 <20", "react-dom": ">=19 <20" });
  assert.deepEqual(manifest.sideEffects, ["*.css"]);
});

test("the entry loads and names the room, its helper and the parts a shell reuses", async () => {
  const room = await import("@vibefield/rosetta-room");
  for (const name of ["HistoryRoom", "catalogStatusOf", "AgentMark", "agentLabel", "lastActive", "hasPrimaryModifier", "shortcutLabel", "MoonGlyph", "SunGlyph"]) {
    assert.ok(room[name], name);
  }
  assert.equal(room.DEFAULT_STORAGE_PREFIX, "ah.");
  const fixtures = await import("@vibefield/rosetta-room/fixtures");
  assert.equal(typeof fixtures.createDemoPorts, "function");
});

test("its imports stay inside it, but for what it declares", () => {
  const leaks = [];
  for (const file of sources) {
    const text = readFileSync(file, "utf8");
    const specifiers = [
      ...text.matchAll(/^\s*(?:import|export)\s[^;]*?\sfrom\s*"([^"]+)"/gms),
      ...text.matchAll(/^\s*import\s*"([^"]+)"/gm),
      ...text.matchAll(/\bimport\(\s*"([^"]+)"\s*\)/g),
    ];
    for (const [, specifier] of specifiers) {
      if (specifier.startsWith(".")) {
        if (!path.resolve(path.dirname(file), specifier).startsWith(SRC)) leaks.push(`${path.relative(SRC, file)} → ${specifier}`);
        continue;
      }
      const name = specifier.startsWith("@") ? specifier.split("/").slice(0, 2).join("/") : specifier.split("/")[0];
      if (!DEPENDENCIES.has(name)) leaks.push(`${path.relative(SRC, file)} → ${specifier}`);
    }
  }
  assert.deepEqual(leaks, []);
});

test("its source dials nothing and keeps nothing outside its prefix", () => {
  const found = [];
  for (const file of sources) {
    const rel = path.relative(SRC, file);
    const code = readFileSync(file, "utf8").replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
    for (const [label, pattern] of [
      ["fetch", /\bfetch\(/], ["socket", /WebSocket|EventSource|XMLHttpRequest/],
      ["bridge", /agentHistoryHost/], ["window message listener", /(?:window|self|globalThis)\.addEventListener\("message"/],
      ["storage", /localStorage|sessionStorage|indexedDB/],
    ]) {
      if (pattern.test(code) && !(label === "storage" && rel === path.join("lib", "room-storage.ts"))) found.push(`${rel}: ${label}`);
    }
  }
  assert.deepEqual(found, []);
});
