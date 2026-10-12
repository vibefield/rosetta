// Emit ESM and declarations without bundling a host's React or dependencies.
// Source imports stay bundler-friendly; emitted imports name real ESM files.
import { cpSync, existsSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";

const root = process.cwd();
const ts = createRequire(path.join(root, "package.json"))("typescript");
const config = ts.readConfigFile(path.join(root, "tsconfig.json"), ts.sys.readFile);
if (config.error) throw new Error(ts.flattenDiagnosticMessageText(config.error.messageText, "\n"));
const source = existsSync(path.join(root, "src")) ? path.join(root, "src") : root;
const output = path.join(root, "dist");
const parsed = ts.parseJsonConfigFileContent(config.config, ts.sys, root, {
  noEmit: false,
  noEmitOnError: true,
  declaration: true,
  rootDir: source,
  outDir: output,
});
rmSync(output, { recursive: true, force: true });
const program = ts.createProgram(parsed.fileNames, parsed.options);
const emitted = program.emit();
const diagnostics = [...parsed.errors, ...ts.getPreEmitDiagnostics(program), ...emitted.diagnostics];
if (diagnostics.length) {
  process.stderr.write(ts.formatDiagnosticsWithColorAndContext(diagnostics, {
    getCanonicalFileName: (file) => file,
    getCurrentDirectory: () => root,
    getNewLine: () => "\n",
  }));
  process.exit(1);
}

function files(dir, skip = []) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    if (skip.includes(entry.name)) return [];
    const file = path.join(dir, entry.name);
    return entry.isDirectory() ? files(file, skip) : [file];
  });
}

// Preserve adjacent styles and the SDK's standalone wire-version entry.
for (const file of files(source, ["node_modules", "dist", "test"])) {
  const relative = path.relative(source, file);
  if (!/\.(css|mjs|d\.mts)$/.test(file)) continue;
  cpSync(file, path.join(output, relative), { recursive: true });
}

for (const file of files(output).filter((file) => /\.(js|d\.ts)$/.test(file))) {
  let text = readFileSync(file, "utf8");
  const syntax = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
  const edits = [];
  function visit(node) {
    const specifier = (ts.isImportDeclaration(node) || ts.isExportDeclaration(node))
      ? node.moduleSpecifier
      : ts.isImportTypeNode(node) && ts.isLiteralTypeNode(node.argument)
        ? node.argument.literal
        : ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword
          ? node.arguments[0] : undefined;
    if (specifier && ts.isStringLiteral(specifier) && /^\.{1,2}\//.test(specifier.text)) {
      const target = path.resolve(path.dirname(file), specifier.text);
      if (!path.extname(specifier.text)) {
        const suffix = existsSync(`${target}.js`) ? ".js" : existsSync(path.join(target, "index.js")) ? "/index.js" : null;
        if (!suffix) throw new Error(`Unresolved emitted import in ${file}: ${specifier.text}`);
        edits.push([specifier.getStart(syntax), specifier.end, JSON.stringify(specifier.text + suffix)]);
      } else if (!existsSync(target)) {
        throw new Error(`Missing emitted import in ${file}: ${specifier.text}`);
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(syntax);
  for (const [start, end, replacement] of edits.sort((a, b) => b[0] - a[0])) {
    text = text.slice(0, start) + replacement + text.slice(end);
  }
  writeFileSync(file, text);
}
console.log(`Built ${path.relative(process.env.INIT_CWD || root, root) || root}`);
