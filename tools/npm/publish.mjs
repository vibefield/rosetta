// Publish previously verified tarballs; permit an exact rerun after a partial release.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const output = path.join(root, "dist/npm");
const packages = JSON.parse(readFileSync(path.join(output, "packages.json"), "utf8"));
assert.deepEqual(packages.map((pkg) => pkg.name), ["@vibefield/rosetta-protocol", "@vibefield/rosetta-room"]);
const registry = "--registry=https://registry.npmjs.org/";
async function publishedIntegrity(pkg) {
  // npm view reads the whole package listing, which can retain a cached 404
  // after a first publish. Query the exact public version instead.
  const response = await fetch(`https://registry.npmjs.org/${pkg.name}/${encodeURIComponent(pkg.version)}`, {
    headers: { accept: "application/json", "cache-control": "no-cache" },
    signal: AbortSignal.timeout(30_000),
  });
  if (response.status === 404) return null;
  assert.ok(response.ok, `Cannot query ${pkg.name}@${pkg.version}: HTTP ${response.status}`);
  const metadata = await response.json();
  assert.equal(metadata.name, pkg.name);
  assert.equal(metadata.version, pkg.version);
  assert.equal(typeof metadata.dist?.integrity, "string", `Unexpected registry response for ${pkg.name}`);
  return metadata.dist.integrity;
}
// Preflight both packages before the first registry mutation.
const pending = [];
for (const pkg of packages) {
  const tarball = path.join(output, path.basename(pkg.filename));
  const integrity = `sha512-${createHash("sha512").update(readFileSync(tarball)).digest("base64")}`;
  assert.equal(integrity, pkg.integrity, `Tarball changed: ${pkg.name}`);
  const existing = await publishedIntegrity(pkg);
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
  for (let attempt = 0; attempt < 60; attempt++) {
    const integrity = await publishedIntegrity(pkg);
    if (integrity) {
      assert.equal(integrity, pkg.integrity, `Registry integrity mismatch: ${pkg.name}`);
      verified = true;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 5000));
  }
  assert.ok(verified, `${pkg.name} not yet readable from npm; rerun this release to resume`);
  console.log(`Verified ${pkg.name}@${pkg.version} on npm`);
}
