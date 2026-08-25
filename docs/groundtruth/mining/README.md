# Spaghetti mining reports (raw, unvetted)

Raw output salvaged from exploration agents that mined
`p008/spaghetti` for Claude Code schema knowledge (2026-08-25). These are
**provenance documents, not curated truth**: agent-written, line citations
unverified, may contain drafting noise. The curated distillation lives in
`crates/adapters/claude-code/GROUND_TRUTH.md` — update that file, cite these.

- `spag-rust-a25757325329e2208.md` — the 14 non-transcript stream specs from
  `spaghetti-napi` (`adapter.rs` StreamSpecs, sidecar decoders, verbatim
  structs, filename→id decoding, redaction notes).
- `spag-rust-a1918ed85e0d8ae41.md` — subagent transcript handling: globs,
  `^agent-(a.+)\.jsonl$` regex divergences (113-of-638 linkage failure),
  typed-parse-only subagent path, workflow journals, slug probing.
- `spag-rust-a1f464546103877db.md` — RFC 012 implementation-plan notes.
- `spag-rust-a1b5d737284f3ef19.md` — RFC 011 engine-doc notes.

The TypeScript-side mining report (transcript schema, 20 record types, 13
system subtypes, TS↔Rust divergences) was delivered in-session and is fully
folded into GROUND_TRUTH.md.
