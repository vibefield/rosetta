# @vibefield/rosetta-protocol

Install with `npm install @vibefield/rosetta-protocol`. The package ships ESM JavaScript and TypeScript declarations. Its npm version is independent of the wire protocol version.

The wire types are generated from `crates/protocol` (ts-rs). Do not edit by hand — regenerate with:

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

`exports` is exactly three entries, and they are the whole public surface:

- `.` — the client plus the generated types. `client.ts` uses `fetch`,
  `WebSocket`, `AbortController`, and `URLSearchParams`, so a consumer of this
  entry needs `DOM` in its `lib`.
- `./protocol-version` — the bare wire number and nothing else. It carries no
  DOM dependency, so a supervisor that only checks a spawned daemon's version
  imports it here instead of dragging the browser client into its typecheck.
  TypeScript resolves the adjacent `protocol-version.d.mts` for the `.mjs`
  under `moduleResolution` `Bundler` or `Node16`.
- `./grants` — minting and verifying per-session grants (`canonicalize`,
  `mintGrant`, `verifyGrant`, `parseGrantHeader`). Node-only: it signs with
  `node:crypto`, so a browser bundle never imports it. `node-crypto.d.ts`
  declares the two functions it uses, so `tsc -p clients/ts` needs no
  `@types/node`.

Everything else is encapsulated by `exports`: there are no deep
imports of `./client` or `./generated/*`, and adding one is a change to this
file, not to a consumer.

## Credentials

A daemon started with `--token-file` answers 401 to any request without a
credential (docs/AUTH.md). Pass one as `new RosettaClient({ credential })`,
or swap it later with `setCredential`:

- `{ kind: "bearer", token }` opens every route.
- `{ kind: "grant", grant }` opens one session's recorded history until the
  grant's `exp` — and that session's recorded-file read (`readFileTarget`)
  when it was minted with `SCOPE_FILES` beside `SCOPE_READ`. The daemon's
  supervisor mints it with `@vibefield/rosetta-protocol/grants`; the client carries it
  as an opaque string.

REST requests send it as `Authorization`. WebSocket upgrades send it as the
subprotocol pair `rosetta` + `rosetta.bearer.<base64url(token)>` or
`rosetta.grant.<grant>`, because a browser's WebSocket cannot set headers;
the daemon selects `rosetta`. A 401 throws `UnauthorizedError`, whose
`reason` is the daemon's `x-rosetta-auth` header. An open socket is not
re-checked, so a subscription outlives its grant's expiry until it
reconnects.

`npm ci` at the repository root installs the workspaces. `npm test` in this
directory builds the SDK and runs `test/*.test.mjs` against its emitted JavaScript;
the grant tests read the shared vectors in
`crates/server/tests/fixtures/grants.json`.
