// The room's sources import one another without extensions, as a bundler and
// a consumer's TypeScript expect. Node's ESM loader resolves none, so a test
// that runs those modules straight under type stripping registers this first:
// `node --import ./test/support/resolve-ts.mjs --test …`.
//
// - Relative specifiers that name no file of their own are retried with the
//   extensions the room's sources use.
// - `@vibefield/rosetta-room` is the package's source, as the app's Vite alias and
//   tsconfig paths resolve it; its `.tsx` modules are transpiled (type
//   stripping takes no JSX) and its stylesheets load as nothing.
import { readFileSync } from "node:fs";
import { createRequire, registerHooks } from "node:module";
import { fileURLToPath } from "node:url";

const CANDIDATES = [".ts", ".tsx", "/index.ts"];
const NAMED = /\.(?:[cm]?[jt]sx?|json|css)$/;
const PACKAGE = {
  "@vibefield/rosetta-protocol": new URL("../../../ts/index.ts", import.meta.url).href,
  "@vibefield/rosetta-room": new URL("../../../room/src/index.ts", import.meta.url).href,
  "@vibefield/rosetta-room/fixtures": new URL("../../../room/src/fixtures/index.ts", import.meta.url).href,
};
let typescript = null;
const ts = () => (typescript ??= createRequire(import.meta.url)("typescript"));

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier in PACKAGE) return { url: PACKAGE[specifier], shortCircuit: true };
    if (!/^\.{1,2}\//.test(specifier) || (NAMED.test(specifier) && !specifier.endsWith(".js"))) return nextResolve(specifier, context);
    try {
      return nextResolve(specifier, context);
    } catch (error) {
      const code = /** @type {{ code?: string }} */ (error).code;
      if (code !== "ERR_MODULE_NOT_FOUND" && code !== "ERR_UNSUPPORTED_DIR_IMPORT") throw error;
      const base = specifier.endsWith(".js") ? specifier.slice(0, -3) : specifier;
      for (const suffix of CANDIDATES) {
        try {
          return nextResolve(`${base}${suffix}`, context);
        } catch {
          // try the next spelling
        }
      }
      throw error;
    }
  },
  load(url, context, nextLoad) {
    if (url.startsWith("file:") && url.endsWith(".css")) return { format: "module", source: "", shortCircuit: true };
    if (url.startsWith("file:") && url.endsWith(".tsx")) {
      const path = fileURLToPath(url);
      const { outputText } = ts().transpileModule(readFileSync(path, "utf8"), {
        fileName: path,
        compilerOptions: { module: ts().ModuleKind.ESNext, target: ts().ScriptTarget.ES2022, jsx: ts().JsxEmit.ReactJSX },
      });
      return { format: "module", source: outputText, shortCircuit: true };
    }
    return nextLoad(url, context);
  },
});
