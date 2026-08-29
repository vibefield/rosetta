// The language tables. Data, not code: adding a language is an entry here,
// never a new branch in the tokenizer.
//
// The keyword lists are deliberately the language's OWN words — its
// declarations, control flow and modifiers — not its standard library. A
// highlighter that paints every library name it has heard of is making a
// claim about the code it cannot support, and it goes stale the first time
// the library does.

import type { Grammar, StringRule } from "./types";

const words = (list: string): ReadonlySet<string> => new Set(list.trim().split(/\s+/));

/** `"…"` and `'…'`, backslash-escaped, one line each. */
const QUOTED: readonly StringRule[] = [
  { open: '"', escape: "\\" },
  { open: "'", escape: "\\" },
];

const SHARED_CONSTANTS = words("true false null undefined nil None True False NULL nullptr");

/* ── the C family ──────────────────────────────────────────────────────── */

export const typescript: Grammar = {
  lineComments: ["//"],
  blockComments: [["/*", "*/"]],
  strings: [{ open: "`", multiline: true, escape: "\\" }, ...QUOTED],
  keywords: words(`
    abstract as async await break case catch class const continue debugger declare default
    delete do else enum export extends finally for from function get if implements import in
    infer instanceof interface is keyof let namespace new of override package private protected
    public readonly require return satisfies set static super switch this throw try type typeof
    var void while with yield
  `),
  constants: SHARED_CONSTANTS,
  types: words(`
    any bigint boolean never number object string symbol unknown void
    Array Promise Record Map Set ReadonlyArray Partial Readonly
  `),
  capitalIsType: true,
  annotation: /@[A-Za-z_][A-Za-z0-9_.]*/y,
  regexLiterals: true,
};

export const rust: Grammar = {
  lineComments: ["//"],
  blockComments: [["/*", "*/"]],
  // Rust string literals span lines, and `'a` is a lifetime rather than the
  // opening of a character literal.
  strings: [{ open: '"', multiline: true, escape: "\\" }, { open: "'", escape: "\\" }],
  lifetimeQuote: true,
  keywords: words(`
    as async await box break const continue crate dyn else enum extern fn for if impl in let
    loop match mod move mut pub ref return self Self static struct super trait type union
    unsafe use where while
  `),
  constants: words("true false None Some Ok Err"),
  types: words(`
    bool char f32 f64 i8 i16 i32 i64 i128 isize str u8 u16 u32 u64 u128 usize
    String Vec Option Result Box Rc Arc HashMap BTreeMap
  `),
  capitalIsType: true,
};

export const python: Grammar = {
  lineComments: ["#"],
  // Docstrings first: `"""` must be tried before `"`, or every docstring
  // opens and closes on its own first two characters.
  strings: [
    { open: '"""', multiline: true, escape: "\\" },
    { open: "'''", multiline: true, escape: "\\" },
    ...QUOTED,
  ],
  keywords: words(`
    and as assert async await break class continue def del elif else except finally for from
    global if import in is lambda match nonlocal not or pass raise return try while with yield
  `),
  constants: words("True False None NotImplemented Ellipsis self cls"),
  types: words("bool bytes dict float frozenset int list object set str tuple type"),
  capitalIsType: true,
  annotation: /@[A-Za-z_][A-Za-z0-9_.]*/y,
};

export const go: Grammar = {
  lineComments: ["//"],
  blockComments: [["/*", "*/"]],
  strings: [{ open: "`", multiline: true }, ...QUOTED],
  keywords: words(`
    break case chan const continue default defer else fallthrough for func go goto if import
    interface map package range return select struct switch type var
  `),
  constants: words("true false nil iota"),
  types: words(`
    bool byte complex64 complex128 error float32 float64 int int8 int16 int32 int64 rune string
    uint uint8 uint16 uint32 uint64 uintptr any
  `),
  capitalIsType: true,
};

export const c: Grammar = {
  lineComments: ["//"],
  blockComments: [["/*", "*/"]],
  strings: QUOTED,
  // `#include`, `#define` — the preprocessor is a second language spoken in
  // the first one's file, and it earns its own mark.
  annotation: /#[ \t]*[a-z]+/y,
  keywords: words(`
    alignas alignof auto break case catch class const consteval constexpr continue co_await
    co_return co_yield decltype default delete do else enum explicit export extern final for
    friend goto if inline mutable namespace new noexcept operator override private protected
    public register return signed sizeof static static_assert struct switch template this
    thread_local throw try typedef typename union unsigned using virtual volatile while
  `),
  constants: words("true false NULL nullptr"),
  types: words("bool char double float int long short size_t ssize_t void wchar_t uint8_t uint16_t uint32_t uint64_t int8_t int16_t int32_t int64_t"),
  capitalIsType: true,
};

export const java: Grammar = {
  lineComments: ["//"],
  blockComments: [["/*", "*/"]],
  strings: QUOTED,
  annotation: /@[A-Za-z_][A-Za-z0-9_.]*/y,
  keywords: words(`
    abstract as assert break by case catch class companion const constructor continue crossinline
    data default do else enum extends external final finally for fun goto if implements import
    in infix init inline instanceof interface internal is lateinit native new object open
    operator out override package private protected public reified return sealed static
    strictfp super suspend switch synchronized tailrec this throw throws transient try typealias
    val var vararg volatile when while
  `),
  constants: words("true false null this super it"),
  types: words(`
    boolean byte char double float int long short void var
    Any Boolean Byte Char Double Float Int Long Nothing Short String Unit
  `),
  capitalIsType: true,
};

export const swift: Grammar = {
  lineComments: ["//"],
  blockComments: [["/*", "*/"]],
  strings: [{ open: '"""', multiline: true, escape: "\\" }, { open: '"', escape: "\\" }],
  annotation: /@[A-Za-z_][A-Za-z0-9_]*/y,
  keywords: words(`
    actor as associatedtype async await break case catch class continue convenience default
    defer deinit didSet do else enum extension fallthrough fileprivate final for func get guard
    if import in indirect infix init inout internal is lazy let mutating nonisolated open
    operator private protocol public repeat required rethrows return self set some static
    struct subscript super switch throw throws try typealias var weak where while willSet
  `),
  constants: words("true false nil"),
  types: words("Any Array Bool Character Data Dictionary Double Float Int Optional Result Set String UInt Void"),
  capitalIsType: true,
};

export const ruby: Grammar = {
  lineComments: ["#"],
  strings: QUOTED,
  variableSigils: ["@@", "@", "$"],
  keywords: words(`
    alias and begin break case class def defined? do else elsif end ensure for if in module
    next not or redo rescue retry return self super then unless until when while yield
  `),
  constants: words("true false nil __FILE__ __LINE__"),
  capitalIsType: true,
};

export const php: Grammar = {
  lineComments: ["//", "#"],
  blockComments: [["/*", "*/"]],
  strings: QUOTED,
  variableSigils: ["$"],
  keywords: words(`
    abstract and array as break callable case catch class clone const continue declare default
    do echo else elseif empty enddeclare endfor endforeach endif endswitch endwhile enum extends
    final finally fn for foreach function global goto if implements include include_once
    instanceof insteadof interface isset list match namespace new or print private protected
    public readonly require require_once return static switch throw trait try unset use var
    while xor yield
  `),
  constants: words("true false null TRUE FALSE NULL"),
  capitalIsType: true,
};

export const shell: Grammar = {
  lineComments: ["#"],
  strings: [{ open: '"', multiline: true, escape: "\\" }, { open: "'", multiline: true }],
  variableSigils: ["$"],
  keywords: words(`
    alias break case cd continue do done echo elif else esac exec exit export fi for function
    if in local return set shift source then time trap unset until while
  `),
  constants: words("true false"),
};

export const sql: Grammar = {
  lineComments: ["--"],
  blockComments: [["/*", "*/"]],
  // SQL escapes a quote by doubling it, which a backslash rule gets wrong;
  // no escape character is the honest setting.
  strings: [{ open: "'" }, { open: '"' }],
  keywords: words(`
    ADD ALL ALTER AND AS ASC AUTOINCREMENT BEGIN BETWEEN BY CASCADE CASE CHECK COLLATE COLUMN
    COMMIT CONFLICT CONSTRAINT CREATE CROSS DEFAULT DELETE DESC DISTINCT DROP ELSE END EXCEPT
    EXISTS FOREIGN FROM FULL GROUP HAVING IF IN INDEX INNER INSERT INTERSECT INTO IS JOIN KEY
    LEFT LIKE LIMIT NOT NULL OFFSET ON OR ORDER OUTER PRAGMA PRIMARY REFERENCES REPLACE RETURNING
    RIGHT ROLLBACK SELECT SET TABLE THEN TRANSACTION UNION UNIQUE UPDATE USING VACUUM VALUES
    VIEW WHEN WHERE WITH
    add all alter and as asc begin between by case check collate column commit constraint create
    cross default delete desc distinct drop else end except exists foreign from full group having
    if in index inner insert intersect into is join key left like limit not null offset on or
    order outer pragma primary references replace returning right rollback select set table then
    transaction union unique update using values view when where with
  `),
  constants: words("TRUE FALSE NULL true false null"),
};

export const lua: Grammar = {
  lineComments: ["--"],
  blockComments: [["--[[", "]]"]],
  strings: [{ open: "[[", close: "]]", multiline: true }, ...QUOTED],
  keywords: words(`
    and break do else elseif end for function goto if in local not or repeat return then until
    while
  `),
  constants: words("true false nil self"),
};

export const json: Grammar = {
  // JSONC and every `tsconfig.json` in the wild. Plain JSON has no comments,
  // and a highlighter that shows one anyway is showing the file it has.
  lineComments: ["//"],
  blockComments: [["/*", "*/"]],
  strings: [{ open: '"', escape: "\\" }],
  constants: words("true false null"),
  propertyBeforeColon: true,
};

/** Text with no grammar at all: no comments, no keywords, nothing claimed. */
export const plain: Grammar = {};
