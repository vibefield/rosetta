// Which view a file gets, and what the dialog is allowed to do with it.
//
// One table, read once per open. Adding a format is an entry here — never a
// branch inside a view, and never a condition in the dialog, which is the
// point of the whole arrangement: the dialog knows there are views, and it
// does not know what any of them are.

import { extensionOf, languageFor, PLAIN_TEXT } from "./syntax";
import type { FileKind, ViewerDefinition } from "./types";
import { BinaryView } from "./views/BinaryView";
import { CodeView } from "./views/CodeView";
import { DiffView } from "./views/DiffView";
import { ImageView, IMAGE_MODES } from "./views/ImageView";
import { MarkdownView, MARKDOWN_MODES } from "./views/MarkdownView";
import { SvgView, SVG_MODES } from "./views/SvgView";
import { imageMediaTypeFromBytes } from "../../lib/image-media";

const KB = 1024;
const MB = 1024 * KB;

/** Source is read whole so the highlighter can see block comments across the
 * file. Two megabytes of it is a very large source file and a fast read; past
 * that the honest move is to say the number rather than to freeze. */
const TEXT_LIMIT = 2 * MB;
/** An image is decoded by the compositor, not by us, so it can be larger. */
const IMAGE_LIMIT = 32 * MB;
/** A hex dump shows its first pages, but the read still crosses a port. */
const BINARY_LIMIT = 8 * MB;

const CODE: ViewerDefinition = {
  id: "code",
  payload: "text",
  maxBytes: TEXT_LIMIT,
  actions: ["copy-path"],
  Body: CodeView,
};

/** HTML is shown as source and never rendered. A page in a worktree is
 * untrusted markup with scripts in it, and this room has a preload bridge; the
 * place that can safely run it is a browser, which is what the extra act
 * offers. That the safe answer and the useful one agree here is luck worth
 * taking. */
const HTML: ViewerDefinition = {
  ...CODE,
  id: "html",
  actions: ["copy-path", "open-in-browser"],
};

const MARKDOWN: ViewerDefinition = {
  id: "markdown",
  payload: "text",
  maxBytes: TEXT_LIMIT,
  modes: MARKDOWN_MODES,
  actions: ["copy-path"],
  Body: MarkdownView,
};

const IMAGE: ViewerDefinition = {
  id: "image",
  payload: "bytes",
  maxBytes: IMAGE_LIMIT,
  modes: IMAGE_MODES,
  actions: ["copy-path"],
  Body: ImageView,
};

/** SVG takes `text` as well as bytes, because its source half is the same
 * code view every other file gets. */
const SVG: ViewerDefinition = {
  id: "svg",
  payload: "text",
  maxBytes: TEXT_LIMIT,
  modes: SVG_MODES,
  actions: ["copy-path", "open-in-browser"],
  Body: SvgView,
};

/** A change, not a file. Its limit is `Infinity` because git bounds its own
 * output host-side and a diff's size has nothing to do with the file's. */
const DIFF: ViewerDefinition = {
  id: "diff",
  payload: "compare",
  maxBytes: Number.POSITIVE_INFINITY,
  actions: ["copy-path"],
  Body: DiffView,
};

const BINARY: ViewerDefinition = {
  id: "binary",
  payload: "bytes",
  maxBytes: BINARY_LIMIT,
  actions: ["copy-path"],
  Body: BinaryView,
};

const IMAGE_TYPES: ReadonlyMap<string, string> = new Map([
  ["png", "image/png"],
  ["jpg", "image/jpeg"],
  ["jpeg", "image/jpeg"],
  ["gif", "image/gif"],
  ["webp", "image/webp"],
  ["avif", "image/avif"],
  ["bmp", "image/bmp"],
  ["ico", "image/x-icon"],
  ["apng", "image/apng"],
]);

/** Extensions worth refusing to decode as text before reading a byte. An
 * unknown extension is NOT on this list: it is read as text and downgraded if
 * the bytes disagree, because most unfamiliar extensions in a repository are
 * somebody's config file. */
const BINARY_EXTENSIONS: ReadonlySet<string> = new Set([
  "zip", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "tar", "jar", "war",
  "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "key", "numbers", "pages",
  "so", "dylib", "dll", "a", "o", "obj", "exe", "bin", "wasm", "node", "rlib", "pdb",
  "woff", "woff2", "ttf", "otf", "eot",
  "mp3", "mp4", "m4a", "wav", "aiff", "flac", "ogg", "webm", "mov", "avi", "mkv",
  "db", "sqlite", "sqlite3", "rdb", "pack", "idx", "class", "pyc", "pyo",
  "psd", "sketch", "fig", "ai", "heic", "tiff", "tif", "dmg", "iso", "img",
]);

export const BINARY_KIND: FileKind = {
  viewer: BINARY,
  label: "Binary",
  language: null,
  mime: null,
};

/** Text with no language behind it: read, shown, nothing claimed about it. */
const UNKNOWN_TEXT_KIND: FileKind = {
  viewer: CODE,
  label: "Text",
  language: PLAIN_TEXT,
  mime: null,
};

/**
 * The kind for a change rather than a file.
 *
 * Chosen by the CALLER, not by the name: the same `main.rs` is a source file
 * when the tree opens it and a diff when the git column does, and only the
 * caller knows which act it is serving.
 */
export function diffKind(name: string): FileKind {
  return { viewer: DIFF, label: "Diff", language: languageFor(name), mime: null };
}

/** What a file name says the file is. Extension only — see `syntax/languages`
 * for why the bytes get no vote here. */
export function resolveKind(name: string): FileKind {
  const extension = extensionOf(name);

  if (extension === "svg") {
    return { viewer: SVG, label: "SVG", language: languageFor(name), mime: "image/svg+xml" };
  }

  const mime = IMAGE_TYPES.get(extension);
  if (mime !== undefined) {
    return { viewer: IMAGE, label: extension.toUpperCase(), language: null, mime };
  }

  if (BINARY_EXTENSIONS.has(extension)) {
    return { ...BINARY_KIND, label: extension === "" ? "Binary" : extension.toUpperCase() };
  }

  const language = languageFor(name);
  if (language === null) return UNKNOWN_TEXT_KIND;

  if (language.id === "markdown") {
    return { viewer: MARKDOWN, label: language.label, language, mime: null };
  }
  if (language.id === "html") {
    return { viewer: HTML, label: language.label, language, mime: "text/html" };
  }
  return { viewer: CODE, label: language.label, language, mime: null };
}

/** Only for a source-proven image slot; extensionless tool captures need the
 * same format hint whether opened from a loaded tile or reached with Next. */
export function imageKind(name: string, bytes?: Uint8Array): FileKind {
  const kind = resolveKind(name);
  if (kind.viewer.id === "image" || kind.viewer.id === "svg") return kind;
  const mime = bytes ? imageMediaTypeFromBytes(bytes) : null;
  if (mime === "image/svg+xml") return resolveKind("image.svg");
  return { viewer: IMAGE, label: mime?.split("/")[1]!.toUpperCase() ?? "Image", language: null, mime };
}

/**
 * Does this look like something no one can read as text?
 *
 * The test git has used for decades: a NUL byte in the first pages. It is a
 * heuristic and it is named as one — but it is the heuristic that is right
 * about real files, and the alternative is showing a reader a screenful of
 * replacement characters and calling it a document.
 */
export function looksBinary(bytes: Uint8Array): boolean {
  const end = Math.min(bytes.length, 8 * KB);
  for (let at = 0; at < end; at += 1) {
    if (bytes[at] === 0) return true;
  }
  return false;
}
