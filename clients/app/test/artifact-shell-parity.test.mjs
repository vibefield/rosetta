import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { artifactDocument } from "../../room/src/components/artifacts/preview.ts";

// The export door's served shell (RO-32) is the room's preview shell rebuilt
// in Rust, pinned in crates/core/tests/golden/served-shell.json. The two may
// differ only where an exported page is served from an origin of its own
// instead of an opaque srcdoc frame: 'self' joins the sources, connect-src is
// 'self' rather than 'none', and base-uri is 'self' rather than the room's
// https://artifact.invalid pin. Anything else that drifts fails here, so a
// change to preview.ts is a change to the shell's version too.
const shell = JSON.parse(
  readFileSync(new URL("../../../crates/core/tests/golden/served-shell.json", import.meta.url), "utf8"),
);

const roomDocument = artifactDocument("");
const roomPolicy = roomDocument.match(/http-equiv="Content-Security-Policy" content="([^"]*)"/)[1].split("; ");

test("the served shell's policy is the room's policy with only the documented differences", () => {
  const expected = shell.policy.map((directive) => {
    if (directive === "connect-src 'self'") return "connect-src 'none'";
    if (directive === "base-uri 'self'") return "base-uri https://artifact.invalid";
    return directive.replace(" 'self'", "");
  });
  assert.deepEqual(roomPolicy, expected);
});

test("the served shell admits exactly the room's hosts, each once", () => {
  const named = (prefix) =>
    roomPolicy
      .find((directive) => directive.startsWith(prefix))
      .split(" ")
      .filter((source) => source.startsWith("https://"))
      .map((source) => source.slice("https://".length));
  assert.deepEqual(named("script-src "), shell.hosts.script);
  assert.deepEqual(named("style-src "), shell.hosts.style);
  assert.deepEqual(named("font-src "), shell.hosts.font);
  assert.deepEqual(shell.hosts.all, [...new Set([...shell.hosts.script, ...shell.hosts.style, ...shell.hosts.font])]);
});

test("the served shell defines the room's variables and base rules, light only", () => {
  const root = roomDocument.match(/:root \{ ([^}]*) \}/)[1];
  const declarations = root
    .split(";")
    .map((part) => part.trim())
    .filter((part) => part.startsWith("--"))
    .map((part) => part.split(": "));
  assert.deepEqual(declarations, shell.variables);
  assert.ok(root.startsWith("color-scheme: light;"), root);
  assert.ok(roomDocument.includes(shell.base_style), "the base rules match byte for byte");
  assert.equal(typeof shell.shell_version, "string");
});
