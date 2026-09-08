# rosetta protocol — generated TypeScript types

Generated from `crates/protocol` (ts-rs). Do not edit by hand — regenerate with:

```sh
TS_RS_EXPORT_DIR=$PWD/clients/ts/generated TS_RS_LARGE_INT=number \
  cargo test -p rosetta-protocol --features ts
```

`TS_RS_LARGE_INT=number` is deliberate: every 64-bit field on the wire is a
millisecond timestamp, ordinal, or token count — far below 2^53 — and the
transport is plain JSON, so `JSON.parse` hands consumers `number`s.

`client.ts` is the handwritten transport and subscription implementation.
Consumers use its cancellation, reconnect, and cache APIs instead of merging
REST pages with WebSocket state themselves. `SessionSubscription.fetchPage`
holds history; `reveal` exposes its latest patched values; `loadEntry` adds an
exact anchor. All three share the snapshot's generation and reset together.

The client and Electron supervisor import `protocol-version.mjs`. When changing
the wire incompatibly, bump it together with Rust's `PROTOCOL_VERSION`, regenerate
the bindings, and refresh the wire golden. The protocol tests check that both
languages agree. HTTP responses expose `x-rosetta-protocol-version`; WebSockets
announce the same version in `hello`. Mismatches fail with an actionable error.
