---
name: atlas
description: Maintain rosetta's architecture atlas (atlas/*.atlas plus @atlas comments, graded by Project Atlas) — place a new crate, package or directory in a layer and domain, keep selectors right through renames, write rules/contracts/flow steps/decisions that cite rosetta's docs, run `atlas check`, and fix dangling or violated claims.
---

# Atlas workflow

The atlas states what rosetta's code cannot say about itself, and Project Atlas grades every
statement against the code: `verified`, `violated`, `asserted` (declared, nothing checkable),
`dangling` (a selector or reference matches nothing), `stale` (≥ 10 commits touched the code since
the claim's file last changed) or `ambiguous`. Rosetta's own docs stay the authority: the atlas
cites them, never replaces them. Read CLAUDE.md "Atlas" first. The DSL spec is
`$ATLAS/docs/DSL.md` (§3 selectors, §4 statements, §5 in-code, §7 grading, §8 agent guide).

## Where it lives

- `atlas/system.atlas` — system, thesis, `let $agents` (the 9 agent crates).
- `atlas/layers.atlas` — the layer stack (position is rank), the TS packages, the directories.
- `atlas/domains.atlas` — domain claims below crate level (modules, files, TS paths).
- `atlas/components.atlas` — runtime units and how they talk (`via http|ws|spawn|fs|ipc|port|embed|codegen`).
- `atlas/contracts.atlas` — `wire` (four anchors in two crates).
- `atlas/flows.atlas` — `live` and `search-query`: step ids, titles, `via`/`direct`, type-anchored steps.
- `atlas/rules.atlas`, `atlas/decisions.atlas` — cross-cutting rules; decisions with their status.
- In code (`grep -rn '@atlas' crates clients`):
  - every crate root: its layer and domain, and the rules whose scope is that one crate
    (core-is-a-leaf, pipeline-deps, thin-protocol, server-deps, search-seam, rosettad-is-root);
  - contracts at their single anchor: `AgentAdapter`, `IngestOp`, `SearchBackend`;
  - flow steps at the function that does the step (`// @atlas step live.read`);
  - `room-dials-nothing` in the header of `clients/room/src/index.ts`.
- `atlas/atlas.baseline.json` — written only by `atlas baseline`.

## Running it

Atlas lives at `$ATLAS`, default `$HOME/Projects/project100/protos/project-atlas`
(github.com/vibefield/project-atlas, private; clone it there and `npm install`). From the rosetta root:

```sh
ATLAS="${ATLAS:-$HOME/Projects/project100/protos/project-atlas}"
npm --prefix "$ATLAS" run -s atlas -- check "$PWD" --intent "$PWD/atlas"          # what is wrong
npm --prefix "$ATLAS" run -s atlas -- check "$PWD" --intent "$PWD/atlas" --all    # also asserted + verified
npm --prefix "$ATLAS" run -s atlas -- explain "$PWD" --intent "$PWD/atlas" crates/pipeline/src/skim.rs
npm --prefix "$ATLAS" run -s atlas -- explain "$PWD" --intent "$PWD/atlas" 'mod:rosetta_capture::{ledger, segment}'
npx --prefix "$ATLAS" tsx "$ATLAS/packages/engine-intent/src/cli.ts" atlas crates clients   # syntax only, 2 s
```

- Pass `--intent "$PWD/atlas"`: it pins the DSL root and the baseline
  (`atlas/atlas.baseline.json`, beside `system.atlas`) whatever Atlas version you run.
  Relative paths resolve against the directory you run from.
- Rust facts come from `cargo doc`, which compiles and runs rosetta's build scripts with its
  target dir in Atlas's own cache, not `target/`: about 20 s cold, 2–5 s when nothing changed.
  Uncommitted files count.
- The last command parses `atlas/` and every `@atlas` comment without building anything: syntax
  and duplicate ids only, no selector or reference checks.
- `baseline` (`npm --prefix "$ATLAS" run -s atlas -- baseline "$PWD" --intent "$PWD/atlas"`) turns
  today's violations into `known` ones. Run it only when the user decides to accept them, never to
  hide a violation your change caused.

## Reading `check`

```
atlas check · Rosetta @ 4348545 · 381 claims
  339 verified · 37 asserted · 5 violated (5 new, 0 known) · 0 dangling · 0 ambiguous · 0 stale
  coverage: layers 100.0% · domains 100.0% of 120,191 prod lines · rules 17 pass, 3 fail, 2 unchecked

VIOLATED · new (5)
  rule:server-deps  only this -> crate:rosetta-{core, protocol, store}
    at crates/server/src/lib.rs:10 · rule · basis edge
    pkg:cargo/rosetta-server → pkg:cargo/rosetta-media
      crates/server/src/http/images.rs:11  use rosetta_media::{MAX_INPUT_BYTES, PreparedImage};
    → this depends on 3 units outside its allow-list: rosetta-media, rosetta-pricing, rosetta-search (first at crates/server/src/http.rs:32). Add them to the only-list if intended, route the call through a…
```

- Sections in order: VIOLATED · new, VIOLATED · known (baselined), DANGLING, AMBIGUOUS, STALE.
  ASSERTED and VERIFIED are only counted unless you pass `--all`.
- Each claim: `subject  DSL text [severity]`, then `at file:line · kind · basis`, then the
  evidence (`file:line  source`), then `→` the remedy. Every glob prints its match count
  (`crate:rosetta-adapter-* → 10`): read it; a count of 0 is the bug.
- Exit 1 only for a NEW violation of severity `error`. DANGLING, STALE, AMBIGUOUS and `warn`
  rules (module-size) do not fail the check, but you still leave 0 dangling and 0 ambiguous.
- Both coverages are 100%: a drop means new code sits in no layer (also an `exhaustive`
  violation) or in no domain (unclaimed). Place it.
- Findings left visible on purpose for the user to decide: `thin-protocol` (protocol re-exports
  search/store types), `server-deps` (server uses media, pricing, search), `module-size` (14
  files, 2 of them TS), `ingest-op` (`IngestOp::GitCommit` has no producer), and
  `dependency-rule` (fails because of the first two). Do not edit a rule or decision to make
  one of them pass.

## Fixing

- **Dangling selector.** `explain` the selector: each term prints `→ n` and
  `DANGLING: matches nothing` for the bad one. Find where the code went (`git log --follow`,
  grep) and fix the selector in the same change as the rename. A crate's name also sits in other
  crates' `@atlas` lines, often in a brace form (`crate:rosetta-{core, protocol, store}`) that a
  grep for the full name misses: grep the stem. If the code is gone for good, delete the claim
  with it.
- **Dangling `see`.** The message suggests the nearest heading (`Did you mean #…?`). A heading
  rename in the docs dangles every `see` pointing at it: fix them together.
- **"has no code subject".** An `@atlas` comment sits below the last declaration or in the wrong
  place; move it directly above its item.
- **Dependency rule violated.** The evidence names the package edge and its sites. Fix the code
  (route through an allowed crate). The remedy line also offers "Add them to the only-list if
  intended" and an `except …` to paste: an agent never takes either on its own. Widening an
  allow-list or adding an exception changes the design, so the rule and the document it cites
  (ARCHITECTURE.md §8) change together, and that is the user's call; propose it instead. A
  sanctioned exception reads `except <selector> "real reason"` (see `store-sealed`,
  `async-at-edge`, `search-seam`); an `except` that excuses nothing goes stale: delete it.
- **Contract violated.** `implemented-by` / `used-by` / `produced-by … each-variant` list the
  missing package or enum variant: implement or produce it, or delete the dead variant.
- **Step asserted: "The step has no `at` anchor".** You deleted or renamed a function that
  carried `// @atlas step flow.step`. Put the line above the function that now does the work.
  A misspelt `@atlas step` id produces no claim and no error at all, so after touching a flow
  run `check --all` and confirm every step of `flow:live` and `flow:search-query` is verified.
- **Stale.** At least 10 commits touched the claim's code since its file (the `.atlas` file, or
  the source file holding the `@atlas` line) last changed. Re-read the code; fix the statement
  if it no longer holds. Staleness is per file in v1, so it clears when that file is next committed.
- **Planned element landed.** `expects … [warn]` resolving means the thing was built: drop
  `status planned`/`proposed` and `expects`, then bind it.

## Authoring

1. **Crate facts go in the crate root**, at the end of the existing `//!` block, after one blank
   `//!` line. Only an inner doc comment in `lib.rs`/`main.rs` binds the whole package, bin
   targets included; there `this` is the package.

   ```rust
   //! …the crate's existing docs, unchanged…
   //!
   //! @atlas layer orchestration · domain ingest
   //! @atlas rule pipeline-deps "pipeline → {core, capture, store, adapters}" {
   //!   only this -> crate:rosetta-{core, capture, store}, crate:rosetta-adapter-*
   //!   see "docs/ARCHITECTURE.md#8-workspace-layout"
   //! }
   ```

   These lines render into rustdoc, and CI runs `clippy -D warnings` and doctests. So: never
   touch the first doc paragraph; indent continuation lines 2 spaces, never 4+ (an indented
   code block becomes a doctest); no `[…]` (intra-doc link), no `<word` (HTML tag), no bare
   URL, no line starting with `#`, `>`, `-`, `*` or `1.`; when the docs end in a code fence,
   insert after the closing fence.
2. **Everything else in Rust uses `//`**, never `///`: put it directly above the item, above its
   `///` docs and attributes, at the item's indentation. `///` would render into rustdoc, and
   ts-rs copies item docs into `clients/ts/generated/*.ts`.

   ```rust
   // @atlas contract ingest-op "IngestOp: where agent weirdness ends" {
   //   produced-by $agents, crate:rosetta-pipeline each-variant
   //   used-by crate:rosetta-store
   // }
   #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
   pub enum IngestOp {
   ```
3. **TypeScript uses `//`** (a `*/` in a glob would close a block comment).
4. **Header pitfall.** A `//` annotation before a file's first code token binds the file (Rust:
   its module; TS: the file, `index.*` its directory), not the declaration below it. Symbol
   annotations go after the imports. The parse command with `--json` shows each annotation's
   `host.style` (`inner-doc` for crate roots) and `host.fileHeader`; `explain <file>` shows
   what it bound to (`crates/core/src/lib.rs:6 · root pkg:cargo/rosetta-core`).
5. **Standalone files** hold what spans crates: the layer structure, domain claims below crate
   level, components and relations, cross-cutting rules, decisions. Declaring an element in two
   places is a parse error.
6. **Cite rosetta.** Every rule and decision carries `see "<file>#<slug>"`; `code IRn` only when
   CLAUDE.md iron rule n is the source. A slug is the heading lowercased, with every character
   that is not a letter, digit, whitespace or `-` dropped, then each whitespace character turned
   into `-` (no collapsing); repeated headings get `-1`, `-2`; headings inside code fences don't
   count. So `## 4. Layer 2 — Normalizer` → `4-layer-2--normalizer`, and `### 5.1 Transport
   decision: one daemon, localhost HTTP/WS` → `51-transport-decision-one-daemon-localhost-httpws`.
   A non-Markdown file (`docs/ingest-engine.html`) takes no `#slug`.
7. **Style** (DSL §8): ids are slugs; the coarsest true selector (crate > mod > path > sym); one
   statement per line; prose only for what code cannot show, at most two sentences; never restate
   a doc comment; do not annotate every module.

## Worked examples

- **New agent adapter** `crates/adapters/foo` (package `rosetta-adapter-foo`): end its `lib.rs`
  docs with `//! @atlas layer agents · domain normalize`. `$agents` picks it up, so
  `adapters-isolated`, `agents-sealed` and the `adapter` contract now cover it (the contract
  fails until it implements `AgentAdapter`). Its `usage*.rs`, `images.rs` and `spec.rs` are
  claimed by the path globs in `atlas/domains.atlas`. Only `rosettad` may name it.
- **New L2 crate.** Rank matters: `data-flows-up` (`layered`) forbids any edge going up the
  stack, so pick the runtime sub-tier that sits above everything the crate depends on, and add
  its allowed dependencies to the doc's §8 dependency rule if it has one.
- **New npm package or top-level directory.** No comment can bind a whole npm package:
  add `contains pkg:…` or `contains path:dir/` to a layer in `atlas/layers.atlas` and a domain
  in `atlas/domains.atlas`.
- **Splitting a big file** (module-size): `// @atlas step` lines move with their functions;
  `mod:`/`file:`/`sym:` selectors that named the old path, in `atlas/` or in other code's
  `@atlas` lines, dangle. Fix them in the
  same change and check `--all` that the flow's steps are still verified.
- **Monitor lands.** When `mod:rosetta_server::metrics` exists, `domain monitor` reports its
  `expects` as landed: drop `status planned` and `expects`, keep its `touches`, add `contains`.
