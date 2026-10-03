// The room's sources import one another without extensions, as a bundler and
// a consumer's TypeScript expect. Node's ESM loader resolves none, so a test
// that runs those modules straight under type stripping registers this first:
// `node --import ./test/support/resolve-ts.mjs --test …`. Only relative
// specifiers that name no file of their own are retried, and only with the
// extensions the room's sources use.
import { registerHooks } from "node:module";

const CANDIDATES = [".ts", ".tsx", "/index.ts"];
const NAMED = /\.(?:[cm]?[jt]sx?|json|css)$/;

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (!/^\.{1,2}\//.test(specifier) || NAMED.test(specifier)) return nextResolve(specifier, context);
    try {
      return nextResolve(specifier, context);
    } catch (error) {
      const code = /** @type {{ code?: string }} */ (error).code;
      if (code !== "ERR_MODULE_NOT_FOUND" && code !== "ERR_UNSUPPORTED_DIR_IMPORT") throw error;
      for (const suffix of CANDIDATES) {
        try {
          return nextResolve(`${specifier}${suffix}`, context);
        } catch {
          // try the next spelling
        }
      }
      throw error;
    }
  },
});
