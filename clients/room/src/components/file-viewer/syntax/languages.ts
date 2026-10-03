// Which language a file is in, and what to call it.
//
// Extension only — never content sniffing. A viewer that guesses the language
// from the bytes is right most of the time and confidently wrong the rest,
// and the name it prints is the reader's only way to tell which happened. The
// file name is a claim its author made; this table just reads it.

import * as grammars from "./grammars";
import { tokenize } from "./tokenize";
import { tokenizeConfig } from "./tokenize-config";
import { tokenizeCss } from "./tokenize-css";
import { tokenizeMarkup } from "./tokenize-markup";
import type { Grammar, Token } from "./types";

export interface Language {
  readonly id: string;
  /** What the kind chip says: the language's own name, as its users spell it. */
  readonly label: string;
  readonly tokenize: (text: string) => readonly Token[];
}

const withGrammar = (id: string, label: string, grammar: Grammar): Language => ({
  id,
  label,
  tokenize: (text) => tokenize(text, grammar),
});

const language = (id: string, label: string, run: (text: string) => readonly Token[]): Language => ({
  id,
  label,
  tokenize: run,
});

export const PLAIN_TEXT: Language = withGrammar("text", "Text", grammars.plain);

const TYPESCRIPT = withGrammar("typescript", "TypeScript", grammars.typescript);
const JAVASCRIPT = withGrammar("javascript", "JavaScript", grammars.typescript);
const RUST = withGrammar("rust", "Rust", grammars.rust);
const PYTHON = withGrammar("python", "Python", grammars.python);
const GO = withGrammar("go", "Go", grammars.go);
const C = withGrammar("c", "C", grammars.c);
const CPP = withGrammar("cpp", "C++", grammars.c);
const JAVA = withGrammar("java", "Java", grammars.java);
const KOTLIN = withGrammar("kotlin", "Kotlin", grammars.java);
const SWIFT = withGrammar("swift", "Swift", grammars.swift);
const RUBY = withGrammar("ruby", "Ruby", grammars.ruby);
const PHP = withGrammar("php", "PHP", grammars.php);
const SHELL = withGrammar("shell", "Shell", grammars.shell);
const SQL = withGrammar("sql", "SQL", grammars.sql);
const LUA = withGrammar("lua", "Lua", grammars.lua);
const JSON_LANG = withGrammar("json", "JSON", grammars.json);
const CSS = language("css", "CSS", tokenizeCss);
const SCSS = language("scss", "SCSS", tokenizeCss);
const HTML = language("html", "HTML", tokenizeMarkup);
const XML = language("xml", "XML", tokenizeMarkup);
const SVG = language("svg", "SVG", tokenizeMarkup);
const YAML = language("yaml", "YAML", tokenizeConfig);
const TOML = language("toml", "TOML", tokenizeConfig);
const INI = language("ini", "INI", tokenizeConfig);
/** Markdown read as its own source: the renderer is the markdown view, and
 * this is what the `source` switch drops back to. */
const MARKDOWN = language("markdown", "Markdown", (text) => tokenize(text, grammars.plain));

const BY_EXTENSION: ReadonlyMap<string, Language> = new Map([
  ["ts", TYPESCRIPT], ["tsx", TYPESCRIPT], ["mts", TYPESCRIPT], ["cts", TYPESCRIPT],
  ["js", JAVASCRIPT], ["jsx", JAVASCRIPT], ["mjs", JAVASCRIPT], ["cjs", JAVASCRIPT],
  ["rs", RUST],
  ["py", PYTHON], ["pyi", PYTHON], ["pyw", PYTHON],
  ["go", GO],
  ["c", C], ["h", C],
  ["cc", CPP], ["cpp", CPP], ["cxx", CPP], ["hpp", CPP], ["hh", CPP], ["hxx", CPP],
  ["java", JAVA],
  ["kt", KOTLIN], ["kts", KOTLIN],
  ["swift", SWIFT],
  ["rb", RUBY], ["rake", RUBY], ["gemspec", RUBY],
  ["php", PHP],
  ["sh", SHELL], ["bash", SHELL], ["zsh", SHELL], ["fish", SHELL], ["ksh", SHELL],
  ["sql", SQL],
  ["lua", LUA],
  ["json", JSON_LANG], ["jsonc", JSON_LANG], ["json5", JSON_LANG], ["webmanifest", JSON_LANG],
  ["css", CSS],
  ["scss", SCSS], ["sass", SCSS], ["less", SCSS],
  ["html", HTML], ["htm", HTML], ["vue", HTML], ["svelte", HTML],
  ["xml", XML], ["xsl", XML], ["xsd", XML], ["plist", XML], ["storyboard", XML], ["xib", XML],
  ["svg", SVG],
  ["yaml", YAML], ["yml", YAML],
  ["toml", TOML], ["lock", TOML],
  ["ini", INI], ["cfg", INI], ["conf", INI], ["properties", INI], ["editorconfig", INI],
  ["md", MARKDOWN], ["markdown", MARKDOWN], ["mdx", MARKDOWN],
  ["txt", PLAIN_TEXT], ["log", PLAIN_TEXT], ["csv", PLAIN_TEXT], ["tsv", PLAIN_TEXT],
]);

/** Files whose name carries the language and whose extension does not. */
const BY_NAME: ReadonlyMap<string, Language> = new Map([
  ["Makefile", language("make", "Makefile", (text) => tokenize(text, grammars.shell))],
  ["Dockerfile", language("dockerfile", "Dockerfile", (text) => tokenize(text, grammars.shell))],
  ["Gemfile", RUBY],
  ["Rakefile", RUBY],
  ["Podfile", RUBY],
  ["Cargo.lock", TOML],
  [".gitignore", language("gitignore", "Ignore rules", (text) => tokenize(text, grammars.shell))],
  [".gitattributes", PLAIN_TEXT],
  [".npmrc", INI],
  [".env", SHELL],
  ["LICENSE", PLAIN_TEXT],
]);

/** The last dot-segment of a name, lowercased. `""` when there is no dot. */
export function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot <= 0 ? "" : name.slice(dot + 1).toLowerCase();
}

/** The language a file name claims, or `null` when no entry matches — which
 * is a different answer from "plain text", and the caller decides what to do
 * with the difference. */
export function languageFor(name: string): Language | null {
  const byName = BY_NAME.get(name);
  if (byName !== undefined) return byName;
  // `.env.local`, `.eslintrc.json` — the leading dot is part of the name, so
  // the extension search has to start after it.
  const stem = name.startsWith(".") ? name.slice(1) : name;
  const dot = stem.indexOf(".");
  if (dot !== -1) {
    const byPrefix = BY_NAME.get(`.${stem.slice(0, dot)}`);
    if (byPrefix !== undefined && BY_EXTENSION.get(extensionOf(name)) === undefined) {
      return byPrefix;
    }
  }
  return BY_EXTENSION.get(extensionOf(name)) ?? null;
}
