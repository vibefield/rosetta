# RosettaClient (Swift)

The Swift client of the daemon API, for a reader that is not a browser or Node: a phone reading one session's
recorded history and its live tail. It is the TypeScript client's session half (`clients/ts/client.ts`), ported rule
for rule.

## Package shape

- The manifest is the repository's root `Package.swift`. SwiftPM resolves a package by its repository, so a consumer
  pins a release tag of this repository, as npm consumers pin `@vibefield/rosetta-protocol`'s published version.
- One library product, `RosettaClient`, for iOS 18 and macOS 15. It has no dependencies.
- Sources are under `clients/swift/Sources/RosettaClient`, and tests under `clients/swift/Tests/RosettaClientTests`.

## What it reads

- `RosettaClient(baseURL:session:credential:sockets:requestTimeout:notificationInterval:)`. The `URLSession` is
  injected: a consumer whose network lives in-process (a tailnet node's netstack) hands in its own, and every request
  and WebSocket rides it.
- `session(_:)`, `timeline(_:limit:cursor:)` and `timelineEntry(_:timelineId:)`: the routes a grant opens
  (docs/AUTH.md).
- `subscribeSession(_:handlers:)`, the live timeline:
  - a snapshot at a stamp;
  - mutations keyed by `timeline_id`, patches into earlier rows included;
  - `resync` barriers;
  - a reconnect on the backoff (500 ms doubling to 10 s) that resumes from the stamp;
  - `fetchPage`, `reveal` and `loadEntry`, under the subscription's generation.
- Credentials, as in the TypeScript client: `.bearer` or `.grant`, as the `Authorization` header and as the
  WebSocket subprotocol pair. `setCredential(_:)` replaces one for later requests and reconnects. Minting stays in
  Node (`clients/ts/grants.ts`).
- Every answer's `x-rosetta-protocol-version` is checked, and so is every hello's `protocol_version`. A 401 names its
  reason (`x-rosetta-auth`). A rewritten session's cursor is `RosettaError.cursorExpired`. Redirects are refused.

## The wire types

The types are hand-written over `crates/protocol`.
- They are strict where the subscribe protocol keys: a row's `timeline_id`, `session_id` and `ordinal`, a stamp, a
  frame's and a mutation's tag.
- They are whole JSON everywhere else. A row's body, usage, tool and every field a newer daemon adds pass through as
  sent.
- No agent data is parsed here (iron rule 6): rows are the daemon's projection, and the consumer reads them.

They are held to the Rust types by the crate's own wire golden, `crates/protocol/tests/golden/wire.json`, which
`WireGoldenTests` reads. Every frame, mutation, page and row the golden pins must decode here, a row must re-encode to
the object the daemon sent, and `Rosetta.protocolVersion` must be the crate's `PROTOCOL_VERSION`. A wire change that
refreshes the golden therefore reaches this client's tests in the same commit.

## Where it differs from the TypeScript client

- It covers the session subscription alone. The meta and collaboration subscriptions, search and the ingestion
  controls are not ported, and their frames and mutations decode as `.other`.
- It says `syncing` at the daemon's hello. A URLSession WebSocket reports no open without a delegate, and the hello
  is the first thing an open socket hears.
- Its state is the main actor's, as the TypeScript client's is its one thread's, and its handlers are called there.
- Rows that share an ordinal are ordered by `timeline_id`'s code points, not `localeCompare`.

## Tests

```sh
swift test
```

Run from the repository root (`swift test --filter RosettaClientTests` to name it). The tests read the golden from
the checkout, so they run where the repository is.
