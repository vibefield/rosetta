import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";

function release(t, existing = {}, failRead = false, tamper = false) {
  const root = mkdtempSync(path.join(tmpdir(), "rosetta-release-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const dir of ["tools/npm", "dist/npm", "bin"]) mkdirSync(path.join(root, dir), { recursive: true });
  copyFileSync(new URL("./publish.mjs", import.meta.url), path.join(root, "tools/npm/publish.mjs"));
  const packages = ["protocol", "room"].map((suffix) => {
    const bytes = Buffer.from(`test tarball ${suffix}`);
    const filename = `${suffix}.tgz`;
    writeFileSync(path.join(root, "dist/npm", filename), bytes);
    return { name: `@vibefield/rosetta-${suffix}`, version: "0.1.0", filename,
      integrity: `sha512-${createHash("sha512").update(bytes).digest("base64")}` };
  });
  writeFileSync(path.join(root, "dist/npm/packages.json"), JSON.stringify(packages));
  const registry = Object.fromEntries(Object.entries(existing).map(([suffix, value]) => {
    const pkg = packages.find((pkg) => pkg.name.endsWith(`-${suffix}`));
    return [`${pkg.name}@${pkg.version}`, value === "same" ? pkg.integrity : value];
  }));
  const statePath = path.join(root, "registry.json");
  writeFileSync(statePath, JSON.stringify({ registry, writes: [] }));
  const fetchStub = path.join(root, "registry.mjs");
  writeFileSync(fetchStub, `
import { readFileSync } from "node:fs";
globalThis.fetch = async (url) => {
  const state = JSON.parse(readFileSync(process.env.TEST_REGISTRY, "utf8"));
  if (process.env.TEST_READ_FAILURE === "1") return new Response("Forbidden", { status: 403 });
  const match = new URL(url).pathname.match(/^\\/(@vibefield\\/rosetta-(?:protocol|room))\\/([^/]+)$/);
  // A package listing may still return 404 while its exact version is live.
  if (!match) return new Response("Not found", { status: 404 });
  const [, name, version] = match;
  const integrity = state.registry[name + "@" + version];
  return integrity
    ? Response.json({ name, version, dist: { integrity } })
    : new Response("Not found", { status: 404 });
};
`);
  const stub = path.join(root, "bin/npm");
  writeFileSync(stub, `#!/usr/bin/env node
const fs = require("node:fs");
const state = JSON.parse(fs.readFileSync(process.env.TEST_REGISTRY, "utf8"));
const [command, value] = process.argv.slice(2);
if (command === "view") {
  console.log(JSON.stringify({error: {code: "E404"}})); process.exit(1);
} else if (command === "publish") {
  const packages = JSON.parse(fs.readFileSync(require("node:path").join(value, "../packages.json"), "utf8"));
  const pkg = packages.find(p => value.endsWith(p.filename));
  state.registry[pkg.name + "@" + pkg.version] = pkg.integrity;
  state.writes.push(pkg.name);
  fs.writeFileSync(process.env.TEST_REGISTRY, JSON.stringify(state));
} else process.exit(2);
`);
  chmodSync(stub, 0o755);
  if (tamper) writeFileSync(path.join(root, "dist/npm/protocol.tgz"), "changed after verification");
  const result = spawnSync(process.execPath, ["--import", fetchStub, path.join(root, "tools/npm/publish.mjs")], {
    encoding: "utf8", env: { ...process.env, PATH: `${root}/bin${path.delimiter}${process.env.PATH}`,
      TEST_REGISTRY: statePath, TEST_READ_FAILURE: failRead ? "1" : "0" },
  });
  return { ...result, ...JSON.parse(readFileSync(statePath, "utf8")) };
}

test("first release publishes protocol before room and verifies both", (t) => {
  const result = release(t);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.writes, ["@vibefield/rosetta-protocol", "@vibefield/rosetta-room"]);
});
test("partial release resumes only missing packages", (t) => {
  const result = release(t, { protocol: "same" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.writes, ["@vibefield/rosetta-room"]);
});
test("published versions are recognized even while package listings return 404", (t) => {
  const result = release(t, { protocol: "same", room: "same" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.writes, []);
});
for (const [label, existing, failRead, tamper] of [
  ["a conflicting room version blocks even the first publish", { room: "different" }, false, false],
  ["authorization errors are never treated as unpublished versions", {}, true, false],
  ["tampered artifacts cannot publish", {}, false, true],
]) {
  test(label, (t) => {
    const result = release(t, existing, failRead, tamper);
    assert.notEqual(result.status, 0);
    assert.deepEqual(result.writes, []);
  });
}
