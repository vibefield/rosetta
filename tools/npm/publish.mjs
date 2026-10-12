// Publish previously verified tarballs; permit an exact rerun after a partial release.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const output = path.join(root, "dist/npm");
const packages = JSON.parse(readFileSync(path.join(output, "packages.json"), "utf8"));
assert.deepEqual(packages.map((pkg) => pkg.name), ["@vibefield/rosetta-protocol", "@vibefield/rosetta-room"]);
const registry = "--registry=https://registry.npmjs.org/";
function publishedIntegrity(pkg) {
  const result = spawnSync("npm", ["view", `${pkg.name}@${pkg.version}`, "dist.integrity", "--json", "--workspaces=false", registry], { encoding: "utf8" });
  if (result.status === 0) {
    const value = JSON.parse(result.stdout);
    const integrity = Array.isArray(value) ? (assert.equal(value.length, 1), value[0]) : value;
    assert.equal(typeof integrity, "string", `Unexpected registry response for ${pkg.name}`);
    return integrity;
  }
  let response;
  try { response = JSON.parse(result.stdout); } catch { /* propagate auth/network errors below */ }
  if (response?.error?.code === "E404") return null;
  throw new Error(`Cannot query ${pkg.name}: ${result.stderr}`);
}
// Preflight both packages before the first registry mutation.
const pending = [];
for (const pkg of packages) {
  const tarball = path.join(output, path.basename(pkg.filename));
  const integrity = `sha512-${createHash("sha512").update(readFileSync(tarball)).digest("base64")}`;
  assert.equal(integrity, pkg.integrity, `Tarball changed: ${pkg.name}`);
  const existing = publishedIntegrity(pkg);
  if (existing) {
    assert.equal(existing, integrity, `${pkg.name}@${pkg.version} already contains different bytes; bump both package versions`);
    console.log(`${pkg.name}@${pkg.version} already published with matching integrity`);
  } else {
    pending.push({ ...pkg, tarball });
  }
}
for (const pkg of pending) {
  execFileSync("npm", ["publish", pkg.tarball, "--access=public", "--ignore-scripts", "--workspaces=false", "--provenance=false", registry], { stdio: "inherit" });
  let verified = false;
  for (let attempt = 0; attempt < 12; attempt++) {
    const integrity = publishedIntegrity(pkg);
    if (integrity) {
      assert.equal(integrity, pkg.integrity, `Registry integrity mismatch: ${pkg.name}`);
      verified = true;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 2000));
  }
  assert.ok(verified, `${pkg.name} not yet readable from npm; rerun this release to resume`);
  console.log(`Verified ${pkg.name}@${pkg.version} on npm`);
}
