import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = fileURLToPath(new URL("../../", import.meta.url));
const output = path.join(root, "dist/npm");
const packages = ["clients/ts", "clients/room"];
const manifests = packages.map((dir) => JSON.parse(readFileSync(path.join(root, dir, "package.json"), "utf8")));
assert.equal(manifests[0].version, manifests[1].version, "Rosetta npm packages release together");
assert.equal(manifests[1].dependencies[manifests[0].name], `^${manifests[0].version}`);
if (process.env.NPM_RELEASE_TAG) {
  assert.equal(process.env.NPM_RELEASE_TAG, `npm-v${manifests[0].version}`, "Tag must match both npm versions");
}
for (const manifest of manifests) {
  assert.equal(manifest.private, undefined);
  assert.equal(manifest.publishConfig.access, "public");
  for (const version of Object.values(manifest.dependencies ?? {})) {
    assert.doesNotMatch(version, /^(link:|file:|workspace:|git|https?:)/, "Published dependencies must resolve from npm");
  }
}
execFileSync("npm", ["run", "build:packages"], { cwd: root, stdio: "inherit" });
mkdirSync(output, { recursive: true });
const packed = packages.map((dir) => {
  const result = JSON.parse(execFileSync("npm", ["pack", "--json", "--ignore-scripts", "--pack-destination", output], {
    cwd: path.join(root, dir), encoding: "utf8",
  }));
  // npm 12 keys workspace results by package name; npm 11 returned an array.
  const [pack] = Array.isArray(result) ? result : Object.values(result);
  for (const file of pack.files) {
    assert.match(file.path, /^(dist\/|package\.json$|README\.md$)/, `Unexpected tarball file: ${file.path}`);
    assert.doesNotMatch(file.path, /(?:^|\/)(?:node_modules|test|\.env)(?:\/|$)/);
  }
  console.log(`${pack.name}@${pack.version}: ${pack.files.length} files, ${pack.size} bytes`);
  return { name: pack.name, version: pack.version, filename: pack.filename, integrity: pack.integrity };
});
writeFileSync(path.join(output, "packages.json"), JSON.stringify(packed, null, 2) + "\n");
