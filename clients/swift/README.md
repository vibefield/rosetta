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
- A row's stored members (`/api/assets/{name}`, which any live read grant opens: docs/AUTH.md §5.4). A consumer
  reads one out of a row's JSON with `AssetRef(_:)`.
  - `imageAssetURL(_:variant:)` is the TypeScript client's `imageAssetUrl`, rule for rule. A WebP bundle is asked at
    its sized member's own address, and `full` at the asset's. A recorded original, or a bundle missing the variant,
    is asked through `?image=<variant>&v=1`. The variants are `full`, `rail`, `tile`, `tile2x` and `preview`.
  - `image(_:variant:maxBytes:)` reads that address and returns the bytes, never decoded, with their content type
    and the answer's `ETag` when it carried one. Every answer is a WebP the daemon encoded; anything else is refused
    before its body is read.
  - `readTextAsset(_:maxBytes:)` reads an oversized-text member (plain text or a canonical document) as a string.
  - A member's address must be http(s) on the client's own origin, so a row cannot send the credential elsewhere.
- `command(_:timelineId:maxBytes:)`: a command body the timeline deliberately omitted, recovered when its
  disclosure is opened.
- The three reads above are bounded: 32 MiB for an image, 4 MiB for text and a command, unless `maxBytes` says
  otherwise. The answer's headers are judged first, then its `Content-Length` when it states one, then its counted
  body when it does not; past the bound the transfer is cancelled and `RosettaError.tooLarge` carries the size the
  member or the answer stated (nil when none was). `readTextAsset` also refuses by the member's own `byte_len`
  before any request. A read is cancelled with its task.
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
- It reads an image itself. The TypeScript client hands a browser the address; a consumer whose network lives
  in-process has no loader that can reach it. It sends no `If-None-Match`: a sized member's own address answers
  with no `ETag`, and both kinds of address are immutable, so an image is cached by its asset's digest and variant.
- `readTextAsset` is bounded and reads text members only. `readAsset` (any member's raw bytes, whole in memory),
  `readFileTarget` and `imageFileTargetUrl` (the recorded-file route, which needs the `files` scope) are not ported.

## Tests

```sh
swift test
```

Run from the repository root (`swift test --filter RosettaClientTests` to name it). The tests read the golden from
the checkout, so they run where the repository is.
