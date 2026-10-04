# Daemon access control

**Status:** built 2026-09-30 (RO-1 the bearer, RO-10 per-session grants) on
`feat/direct-plane-grants`; rebased onto `main` `886baed` and MERGED 2026-10-03 (tip `ee6724e`,
the branch's five commits intact). The `files` scope (RO-13: a session's recorded-file read
under a grant, §5.4) built 2026-10-04 on `feat/grant-files-scope`. This page is the rule;
`crates/server/src/auth.rs`
and `grant.rs` are its code, `clients/ts/grants.ts` its TypeScript twin, and
`crates/server/tests/fixtures/grants.json` the vectors both reproduce.
ARCHITECTURE §5.1 points here.

## 1. Modes

| `serve` flags | Behaviour |
|---|---|
| none | **Open, as before.** Any process that reaches the listener reads everything. Browsers are held by the Origin rule (§2). |
| `--token-file FILE` | **Bearer.** Every route and every WebSocket upgrade needs a credential (§3). |
| `--token-file FILE --grant-key-file FILE` | **Bearer and grants.** As above, and a grant also opens one session's recorded history until it expires (§5). |
| `--grant-key-file FILE` alone | Refused at startup. A grant stands in for the bearer on one session's routes, so without a bearer there is nothing for it to stand in for. |
| `--allow-origin ORIGIN` (repeatable) | Adds browser origins to the Origin rule, in any mode. |
| `--debug-auth` | One stderr line per grant decision: the client id (once the MAC verified) and the route template. Never the grant, its MAC, or the session id. |

The `--json` ready event carries `"auth": {"bearer": bool, "grants": bool,
"origins": [...]}`, which names the modes and never the secrets.

## 2. Two guards, never one for the other

**The Origin rule guards against web pages.** A page cannot forge its own
`Origin`, so the daemon refuses a foreign origin with **403** wherever browsers
skip CORS: every WebSocket upgrade, the two writes (`/api/runtimes/enable`,
`/api/ingest/boost`), and the recorded-file route. CORS withholds every other
response from a foreign page. The allowed origins are any port on a loopback
host (`http`/`https` on `localhost`, `127.0.0.1`, `[::1]`), `tauri://localhost`,
`rosetta://app`, and each `--allow-origin`. Extra origins are exactly
`scheme://host[:port]`, compared case-insensitively, and `null`, wildcards,
paths, queries and credentials are refused. `null` is never allowed, because
any site can manufacture it.

**The credential guards against processes.** A process that is not a browser
can send any `Origin` it likes, and so can a request relayed from another
device. An allowed `Origin` therefore proves nothing about the caller. With a
bearer configured, a request that passes the Origin rule STILL needs a
credential; without that, `curl -H 'Origin: rosetta://app'` would read
everything. rosetta's own app is unaffected, because it spawns its own daemon
with no `--token-file`. Pointed at a configured daemon, the app would have to
present the token like any other client.

A request with no `Origin` (every non-browser client) passes the Origin rule
and answers to the credential check alone.

## 3. Credentials and carriers

| Credential | `Authorization` header | WebSocket subprotocol pair |
|---|---|---|
| the bearer | `Bearer <token>` | `rosetta`, `rosetta.bearer.<base64url(token)>` |
| a grant | `Rosetta-Grant <grant>` | `rosetta`, `rosetta.grant.<grant>` |

- A browser's WebSocket cannot set `Authorization`, so the credential can
  instead ride `Sec-WebSocket-Protocol`. The daemon selects `rosetta`, so the
  handshake completes, and never echoes the credential entry. A client that
  offers a credential entry must also offer `rosetta`: browsers fail a
  handshake whose server selects no offered protocol.
- The scheme names are case-insensitive. `Authorization` wins when both
  carriers are present. Two credentials in one carrier, or an unknown
  scheme, are `malformed`.
- The bearer is compared as BLAKE3 digests, which is constant-time and leaks
  nothing about the token's length.
- A credential is checked at the request, or at the upgrade. **An open
  WebSocket is not re-checked:** a stream outlives its grant's `exp` until it
  disconnects, and the reconnect then needs a live grant.
- The credential check runs after routing and inside CORS. CORS answers
  preflights without it. With a bearer configured, a preflight mirrors the
  requested headers, because `Access-Control-Allow-Headers: *` never covers
  `Authorization`. The 401's `x-rosetta-auth` and `WWW-Authenticate` headers
  are exposed to allowed pages. With no bearer, the CORS headers are
  byte-for-byte what they were.

## 4. The files

| | `--token-file` | `--grant-key-file` |
|---|---|---|
| content | the bearer, the file trimmed of surrounding ASCII whitespace | the HMAC key: the file's bytes verbatim, no trimming, no decoding |
| bounds | 32 to 1,024 visible ASCII characters (`0x21`–`0x7E`) | at least 32 bytes |
| both | a regular file of at most 4 KiB; on Unix, **no group or other permission bits at all** (`0600` or stricter); anything else is refused with a `chmod 600` hint | |

Both files are read once, before the listener binds, so a bad file is a
startup refusal and nothing is created. A supervisor may delete both files
after the `ready` event. Windows ACLs are not inspected.

## 5. Grants

### 5.1 Format

```
grant   = base64url(payload) "." base64url(mac)
mac     = HMAC-SHA256(key, payload)            ; over the payload BYTES
payload = canonical JSON of {cid, exp, scope, sid, v}   ; §5.2
```

`base64url` is RFC 4648 §5 with no padding, decoded strictly: no padding, no
character outside the URL-safe alphabet, and zero trailing bits, so every byte
string has exactly one spelling.

| member | type | meaning |
|---|---|---|
| `cid` | non-empty string | the minter's name for the client. Logged with `--debug-auth`; nothing is bound to it |
| `exp` | integer, `0 ≤ exp ≤ 2^53 − 1` | expiry in Unix milliseconds on the daemon's clock |
| `scope` | array of strings | what the grant may do. `"read"` opens the session's recorded history and the assets, and every grant-openable route needs it; `"files"` beside it also opens the session's recorded-file read (§5.4). Unknown entries are carried and ignored |
| `sid` | non-empty string | the canonical session id the grant opens |
| `v` | `1` | the payload version |

### 5.2 The canonical rule

The payload is the RFC 8785 (JCS) serialization of exactly those five
members. For this shape that means:

- the members appear in the order `cid`, `exp`, `scope`, `sid`, `v` (RFC 8785
  sorts by UTF-16 code unit, and every name is ASCII);
- there is no whitespace anywhere;
- strings are escaped exactly as ECMAScript's `JSON.stringify` writes them:
  `\"`, `\\`, the short forms `\b \f \n \r \t`, and every other U+0000–U+001F
  as `\u00xx` in lowercase hex. Everything else is raw UTF-8, including `/`,
  U+007F, U+2028 and every non-ASCII character. A lone surrogate is not text
  and is refused;
- integers are written in plain decimal, with no sign on zero, no fraction and
  no exponent.

The daemon re-encodes the parsed claims and requires the payload to be
byte-identical. Whitespace, another member order, an extra member, `1.0`,
`\/` and the like are all `malformed`, even under a valid MAC. So there is one
accepted spelling of every grant, and no parser differential between minter
and verifier.

Example (the first vector; key = bytes `00 01 … 1f`):

```
payload {"cid":"fieldd/window-1","exp":1790000060000,"scope":["read"],"sid":"s1","v":1}
grant   eyJjaWQiOiJmaWVsZGQvd2luZG93LTEiLCJleHAiOjE3OTAwMDAwNjAwMDAsInNjb3BlIjpbInJlYWQiXSwic2lkIjoiczEiLCJ2IjoxfQ.KhNxqADtPdBGQ6BLRua1sYTUpb6xXmxub2iYQ4mwjzQ
```

### 5.3 Verification order

0. A daemon started without `--grant-key-file` refuses every grant with
   `grants-disabled`.
1. The route. A grant counts only on a `GET`/`HEAD` of a route in §5.4;
   anywhere else it is `wrong-route`, judged before the grant is read.
2. The form. At most 4,096 bytes, split at the first `.`, both halves decoded
   strictly, a MAC of 32 bytes; otherwise `malformed`.
3. **The MAC**, compared in constant time; otherwise `bad-signature`. Nothing
   in the payload is parsed before this step.
4. The payload: version-1 claims that equal their own canonical re-encoding,
   with `v = 1`, non-empty `sid` and `cid`, and `exp` in range; otherwise
   `malformed`.
5. `exp > now`, in Unix milliseconds on the daemon's clock; otherwise `expired`.
6. The route's `{id}` equals `sid`; otherwise `wrong-session` (assets are
   exempt, see §5.4).
7. `scope` contains `"read"` — and, on the recorded-file route, `"files"` as
   well; otherwise `wrong-scope`.

### 5.4 What a grant opens, and what it never opens

An allow-list, by router template. A route added later needs the bearer until
someone decides otherwise. A grant opens, when `{id}` = `sid`:

```
/ws/sessions/{id}                                 /api/sessions/{id}/timeline/{timeline_id}
/ws/collaboration/{id}                            /api/sessions/{id}/timeline/{timeline_id}/command
/api/sessions/{id}                                /api/sessions/{id}/timeline/{timeline_id}/work
/api/sessions/{id}/collaboration                  /api/sessions/{id}/timeline/{timeline_id}/plan
/api/sessions/{id}/timeline                       /api/sessions/{id}/plans/revisions/{revision}
```

It also opens `/api/assets/{name}`, the content-addressed transcript images.
An asset URL is learned from a timeline, and its digest names bytes, not a
session. The cost is that a grant holder who already knows an image's digest
can confirm that the projection holds it.

**With `"files"` beside `"read"`** it also opens, when `{id}` = `sid`,

```
/api/sessions/{id}/timeline/{timeline_id}/read/{target_index}
```

the recorded-file read (with or without its `?image=` rendering), and nothing
else: that is the one route the scope opens. The route reads, from the
daemon's disk, the file the timeline row names — never a path the caller
sends — as it is NOW, not as the transcript recorded it. A grant with `"read"`
alone is `wrong-scope` there.

**Why `"files"` is a scope of its own.** A session's recorded history is what
the agent said and did; the file a row names is whatever that path holds
today, which can be more than the transcript ever showed (a later edit, a
secret written since). So reading recorded history does not imply reading
recorded files, and the minter grants the second separately, per principal:
the supervisor that holds the key decides which of its clients may read files
from this machine's disk (VibeField's fieldd, for one, mints it for its own
desktop window and not for its phone or another desktop). The daemon checks
only that a grant carries it.

**A grant never opens** the lists (`/api/runtimes`, `/api/projects`,
`/api/sessions`), search, `/ws/meta`, `/api/status`, `/api/discoveries`,
`/api/pricing`, the project aggregates or the writes, whatever its scope.
Those routes need the bearer.

### 5.5 What v1 does not do

- **No revocation list.** A grant lives until `exp`. Restarting the daemon with
  a new key revokes every grant at once, and a supervisor that mints the
  bearer per start rotates both together.
- **No maximum lifetime.** The minter sets `exp`; the daemon only refuses a
  past one.
- **One session, matched exactly.** A subagent's timeline is its own session
  id and needs its own grant. A tree rule (open `sid` and the sessions whose
  `root_session_id` is `sid`) is a named upgrade, not built.
- **Checked at the upgrade** (§3).

## 6. Refusals

| Status | When |
|---|---|
| **401** | a credential is required and missing or refused. `x-rosetta-auth: <reason>`, `WWW-Authenticate: Bearer realm="rosetta"` (and `Rosetta-Grant realm="rosetta"` when grants are on), `Cache-Control: no-store`, a `text/plain` body `rosetta: unauthorized (<reason>)`, and the usual `x-rosetta-protocol-version`, so a client can tell "wrong daemon" from "wrong credential" |
| **403** | the Origin rule (§2) |

| Reason | Meaning |
|---|---|
| `missing` | no credential, and this daemon requires one |
| `bad-token` | a bearer that is not this daemon's |
| `malformed` | an unreadable credential: an unknown scheme, two credentials, bad base64, or a grant that is not a canonical version-1 grant |
| `bad-signature` | the grant's MAC does not verify under this daemon's key |
| `expired` | an authentic grant whose `exp` has passed |
| `wrong-session` | an authentic grant for another session |
| `wrong-scope` | an authentic grant without the scope the route needs: `"read"` everywhere, and `"files"` as well on the recorded-file route |
| `wrong-route` | a grant on a route grants never open |
| `grants-disabled` | a grant presented to a daemon started without `--grant-key-file` |

A browser cannot read a refused WebSocket handshake's status. A browser
client that needs the reason can ask a REST route of the same session with
the same credential.

## 7. Threat model

- **Web pages in the user's browser** cannot forge `Origin`, so the Origin
  rule holds them in every mode. One gap remains in the open mode: the daemon
  checks no `Host` header, so a DNS-rebinding page's same-origin `GET`
  carries no `Origin` and is answered. The bearer closes that gap, because the
  page cannot present the token.
- **Processes running as the same user** (agents included) read everything
  in the open mode. With a bearer they must hold the token. A same-uid
  process CAN read a `0600` file it finds, so the file's mode only keeps
  other accounts out. The remaining defence is the supervisor's: keep the
  file where agents are not pointed, delete it after `ready`, and never put
  the token in an agent's environment. Nothing here defends against a
  process that can read the supervisor's memory.
- **Other devices on a private network**, whose requests reach the loopback
  listener through a forwarding route, arrive with any `Origin` their caller
  chose. The Origin rule cannot hold them; the credential does. A grant
  narrows a leaked credential to one session's recorded history until `exp`
  — and, if it carries `files`, the files that session's rows name.
- **Other local accounts** cannot read the `0600` files, but they can still
  connect to the loopback port. The bearer holds them out; the open mode does
  not.
- **Out of scope:** TLS (the listener is loopback; a private network carries
  its own encryption) and rate limiting. Guessing faces a token of at least
  32 characters and a 256-bit MAC.

## 8. Interop

- **TypeScript:** `RosettaClient({ credential })` carries either credential
  on both carriers and throws `UnauthorizedError` with the reason.
  `@rosetta/protocol/grants` (Node-only) provides `canonicalize`,
  `mintGrant`, `verifyGrant` and `parseGrantHeader`, the same rule as §5, and
  the scope names `SCOPE_READ` and `SCOPE_FILES` (`grant.rs` exports the same
  two).
- **Vectors:** `crates/server/tests/fixtures/grants.json` is written by
  `tools/auth/grant_vectors.py`, a stdlib-Python generator independent of both
  implementations (a rerun reproduces it byte for byte). The Rust and
  TypeScript suites both reproduce every valid vector byte for byte, refuse
  every invalid one for its stated reason, and list the §6 reasons exactly as
  the fixture's `refusals`.
- **Protocol version:** unchanged. Access control adds headers and opt-in
  modes; no route or frame shape moved.
