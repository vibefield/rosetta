// Test the actual tarballs in a clean consumer, with no workspace links or TS loader.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const published = process.argv.includes("--published");
const consumer = mkdtempSync(path.join(tmpdir(), "rosetta-npm-consumer-"));
const run = (command, args) => execFileSync(command, args, { cwd: consumer, stdio: "inherit" });
const write = (name, value) => writeFileSync(path.join(consumer, name), value);
try {
  if (!published) execFileSync(process.execPath, [path.join(root, "tools/npm/pack.mjs")], { cwd: root, stdio: "inherit" });
  const packages = JSON.parse(readFileSync(path.join(root, "dist/npm/packages.json"), "utf8"));
  write("package.json", JSON.stringify({ private: true, type: "module" }));
  const version = (name) => JSON.parse(readFileSync(path.join(root, "node_modules", name, "package.json"), "utf8")).version;
  run("npm", ["install", "--ignore-scripts", "--no-audit", "--no-fund",
    ...packages.map((pkg) => published ? `${pkg.name}@${pkg.version}` : path.join(root, "dist/npm", pkg.filename)),
    ...["react", "react-dom", "@types/react", "@types/react-dom", "typescript", "vite"].map((name) => `${name}@${version(name)}`),
  ]);
  const vectors = JSON.parse(readFileSync(path.join(root, "crates/server/tests/fixtures/grants.json"), "utf8"));
  write("grants.json", JSON.stringify(vectors));
  write("node-smoke.mjs", `
    import assert from "node:assert/strict";
    import { readFileSync } from "node:fs";
    import { RosettaClient, PROTOCOL_VERSION } from "@vibefield/rosetta-protocol";
    import { PROTOCOL_VERSION as standalone } from "@vibefield/rosetta-protocol/protocol-version";
    import { mintGrant, verifyGrant } from "@vibefield/rosetta-protocol/grants";
    import { createDemoPorts } from "@vibefield/rosetta-room/fixtures";
    assert.equal(PROTOCOL_VERSION, standalone);
    assert.ok(new RosettaClient());
    assert.equal(typeof createDemoPorts().sessions.open, "function");
    const vectors = JSON.parse(readFileSync("grants.json", "utf8"));
    const key = Uint8Array.from(Buffer.from(vectors.key_hex, "hex"));
    for (const vector of vectors.valid) {
      assert.equal(mintGrant(key, vector.claims), vector.grant);
      assert.deepEqual(verifyGrant(key, vector.grant, vectors.now_ms), { ok: true, claims: vector.claims });
    }
    console.log("Packed ESM exports and shared grant vectors passed");
  `);
  run(process.execPath, ["node-smoke.mjs"]);
  write("consumer.tsx", `
    import { createElement } from "react";
    import { createRoot } from "react-dom/client";
    import { RosettaClient, type SessionRow } from "@vibefield/rosetta-protocol";
    import { HistoryRoom, type RoomPorts } from "@vibefield/rosetta-room";
    import { createDemoPorts } from "@vibefield/rosetta-room/fixtures";
    import "@vibefield/rosetta-room/styles/room.css";
    const ports: RoomPorts = createDemoPorts();
    const sessionId = (session: SessionRow): string => session.id;
    console.log(new RosettaClient(), sessionId);
    createRoot(document.getElementById("app")!).render(createElement(HistoryRoom, ports));
  `);
  write("tsconfig.json", JSON.stringify({ compilerOptions: {
    target: "ES2022", module: "NodeNext", moduleResolution: "NodeNext", jsx: "react-jsx",
    lib: ["ES2022", "DOM", "DOM.Iterable"], strict: true, noEmit: true,
    exactOptionalPropertyTypes: true, noUncheckedIndexedAccess: true, types: ["react", "react-dom"],
  }, files: ["consumer.tsx"] }));
  run(process.execPath, ["node_modules/typescript/bin/tsc", "-p", "tsconfig.json"]);
  // The supervisor's entry must typecheck without bringing DOM types along.
  write("supervisor.ts", 'import { PROTOCOL_VERSION } from "@vibefield/rosetta-protocol/protocol-version"; export const version: number = PROTOCOL_VERSION;');
  write("tsconfig.supervisor.json", JSON.stringify({ compilerOptions: {
    target: "ES2022", module: "NodeNext", moduleResolution: "NodeNext", lib: ["ES2022"],
    strict: true, noEmit: true, types: [],
  }, files: ["supervisor.ts"] }));
  run(process.execPath, ["node_modules/typescript/bin/tsc", "-p", "tsconfig.supervisor.json"]);
  write("index.html", '<div id="app"></div><script type="module" src="/consumer.tsx"></script>');
  run(process.execPath, ["node_modules/vite/bin/vite.js", "build"]);
  for (const pkg of packages) {
    const installed = JSON.parse(readFileSync(path.join(consumer, "node_modules", pkg.name, "package.json"), "utf8"));
    assert.equal(installed.version, pkg.version);
  }
  console.log("Clean tarball consumer: ESM, NodeNext declarations, DOM-free version entry, and browser/CSS build passed");
} finally {
  rmSync(consumer, { recursive: true, force: true });
}
