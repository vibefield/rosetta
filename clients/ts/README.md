# rosetta protocol — generated TypeScript types

Generated from `crates/protocol` (ts-rs). Do not edit by hand — regenerate with:

```sh
TS_RS_EXPORT_DIR=$PWD/clients/ts/generated TS_RS_LARGE_INT=number \
  cargo test -p rosetta-protocol --features ts
```

`TS_RS_LARGE_INT=number` is deliberate: every 64-bit field on the wire is a
millisecond timestamp, ordinal, or token count — far below 2^53 — and the
transport is plain JSON, so `JSON.parse` hands consumers `number`s.
