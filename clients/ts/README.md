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

## Package shape

`exports` is exactly two entries, and they are the whole public surface:

- `.` — the client plus the generated types. `client.ts` uses `fetch`,
  `WebSocket`, `AbortController`, and `URLSearchParams`, so a consumer of this
  entry needs `DOM` in its `lib`.
- `./protocol-version` — the bare wire number and nothing else. It carries no
  DOM dependency, so a supervisor that only checks a spawned daemon's version
  imports it here instead of dragging the browser client into its typecheck.
  TypeScript resolves the adjacent `protocol-version.d.mts` for the `.mjs`
  under `moduleResolution` `Bundler` or `Node16`.

`imports["#ordered-cache"]` is internal to the package. Everything else is
encapsulated by `exports`: there are no deep imports of `./client` or
`./generated/*`, and adding one is a change to this file, not to a consumer.
