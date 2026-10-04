# @rosetta/room

rosetta's History room as a package: the projects, sessions and worktree
columns, the summary panels and charts, and the transcript with its folds,
behind injected ports. The room dials nothing. A host hands it a catalog, a
way to open one session's body, an asset source and, on a desktop, a host
port; each host writes its own adapters. rosetta's own app
(`clients/app/renderer/src/host`) is the reference host.

```tsx
import { HistoryRoom } from "@rosetta/room";
import "@rosetta/room/styles/room.css"; // after the host's --vf-* tokens

<HistoryRoom catalog={catalog} sessions={sessions} assets={assets} host={host} />;
```

`@rosetta/room/fixtures` renders the room with no daemon at all:
`<HistoryRoom {...createDemoPorts()} />` (six projects, eleven sessions, a year
of figures, two transcripts with images and web lines, all relative to now).

## The ports (`src/ports.ts`)

| Port | What the room asks | rosetta's app answers with |
|---|---|---|
| `CatalogPort` | `subscribeMeta` (runtimes, projects, every session row, live: one snapshot then mutations) · `status` → `CatalogStatus` · `projectStats(projectId, window)` → `ProjectStats` (per-day heat and per-session facts) · `usage` · `dayUsage` · `search(CatalogSearch)` · optional `onConnectionChange`, `unavailable` (the host's own words for why the index cannot answer) and `boost` | its client's REST and `/ws/meta` (`catalogOver`) |
| `SessionPort` | `open(sessionId)` → a `SessionClient` bound to that session (its live subscription, child threads and team members each open their own; entries, report work, commands, plans, file targets, stored members), or a promise of one; `close(sessionId)` | the one client for every session; close is nothing |
| `AssetSource` | `imageAssetUrl(asset, variant)` → a URL, null, or a promise of either · `attachmentAssetUrl(asset)` → the attachment's address, for identity and can-open only | the daemon's own URLs |
| `HostPort` (optional) | `homeDir` · `platform` · `openInBrowser(name, bytes)` · `favicon(site)` · optional `worktree: { open, gitFacts, fileAction, ports }` | the Electron bridge (`hostPortOver`) |

- **The room opens a session at most once at a time and follows every `open`
  with exactly one `close`**, when nothing in it reads that session any more.
  A host that mints a credential per open mints one per session the reader is
  looking at.
- **A `RosettaClient` is a `SessionClient`** (checked at compile time). A host
  whose daemon wants a per-session credential hands a client that carries it.
- **An `<img src>` cannot carry a credential.** A host whose daemon wants one
  fetches the bytes through a session's client and answers an object URL — a
  promise while it fetches — and bounds and revokes what it made. The room asks
  on every mount and every retry, so an evicted URL is asked for again, never
  reused, and a source must not keep a failure. A null answer draws the
  image's "could not load" face with Retry.
- **`boost` is optional** because a grant never opens a write: only a host
  holding the daemon's bearer offers it. Without it the index keeps its order.
- **Without a host** the room states what a browser tab is: no worktree guest,
  a globe for every site icon, no "open in browser". With a host but no
  `worktree`, the guest stays closed.
- **`worktree.ports`** is the engine's event stream: a generation starting or
  failing, then its two transferred `MessagePort`s (tree and git). A host
  replays what arrived before the room listened; the room is the sole
  consumer.

## What a host decides (`HistoryRoomProps`)

`surface` (`"paper"`, the default, paints the room's own ground and plate;
`"host"` paints neither and turns the paper fades into frost over whatever the
host paints) · `portalContainer` (where the dialogs and the search sheet
portal; their sizes are percentages of what holds them) · `edgeInset`,
`scrollFoot`, `floatingFoot` · `onSelect` (the selected session's id, native id
and folder, on mount and on every change) · `storagePrefix` and `storage` ·
`initialSummaryTab` · `unavailable` and `consent` (the host's words and its
consent screen) · `emptyPrompt` (the empty transcript's sentence in the host's
words; absent, the room's own, below) · `search` (the room's own strip and
sheet, with the host owning the open state; absent, neither is drawn). The ref
handle offers `openAt({ sessionId, timelineId? })` and `stepMatch(delta)`.

**The search key is the host's (RO-14).** The room binds no key of its own:
the host's window opens the sheet (rosetta's app binds ⌘K, Ctrl K off a Mac,
and `/`). So the room prints only a key the host says it binds.
`search.shortcut` decides the legend:

| `search.shortcut` | The launcher prints | The empty transcript says |
|---|---|---|
| absent | the room's own label, ⌘K (Ctrl K off a Mac) | "Pick a session — or press ⌘K to search, which is the front door." |
| `{ label }` | `label`, as given | the same sentence with `label` |
| `null` (the host binds no key) | no legend | "Pick a session." |

With no `search` at all there is no launcher and the sentence is "Pick a
session." A host whose own search has a key of its own (VibeField's Peek)
names it in `emptyPrompt`.

## What the package never does

- **No fetch and no socket.** Every byte arrives through a port.
- **No storage outside the prefix.** Four names — `search.recent`,
  `tintSeq.v1`, `siteIcons.v1`, `transcript.fold.v2` — under `storagePrefix`
  (default `ah.`) in the host's `storage` (default `localStorage`; `null` keeps
  nothing).
- **No window globals.** It reads and writes no custom property of `window`
  and installs no message or key listener on the host's behalf. It reads
  `matchMedia` and `devicePixelRatio`, listens to `focus` and
  `visibilitychange` to refresh its clock, listens while one of its own
  popovers is open, and re-measures its minimap on `resize`. External links
  leave through the browser's own `target="_blank"` and `window.open`, which
  the host's shell governs.
- **No page styles.** Every rule is scoped to the room's root (`.ah-frame`)
  and to the layers it portals (`.ah-layer`), at the specificity it had; the
  page's resets and a global reduced-motion floor are the host's.

## Styles and tokens

The room reads the VibeField design kit's `--vf-*` tokens (the host defines
them) and brings its own: `--ah-*` (`--ah-plate`, `--ah-scroll-foot`,
`--ah-floating-foot`, `--ah-inset-*`), `--tr-*`, and the project tint band
`--tint-l`/`--tint-c`. The root carries `data-ah-surface`, and
`data-ah-overlay` while one of its dialogs or the sheet is open, so a host can
stop its own chrome (a drag region, say) under it.

## Building against it

The package ships TypeScript source. It compiles under `strict` with
`exactOptionalPropertyTypes`, `noUncheckedIndexedAccess` and
`verbatimModuleSyntax` (`pnpm typecheck` here), and its relative imports carry
no extension, so a host's compiler needs no `allowImportingTsExtensions`.
`pnpm install` in this folder brings its dependencies; `@vibecook/mille` and
`mille-ui` are `link:` rows into a sibling mille checkout, as in the app, and
the two Radix menus are there as mille-ui's peers (the room's tree uses them).
React is a peer at `>=19 <20` (the dev and test React is pinned to 19.2.7 — the version
the package's first host, VibeField, resolves; its dependency check requires one React across
its linked siblings, so the pin moves only with that host's): `@vibecook/mille-ui` itself wants React 19, and
the room uses React 19's boolean `inert`, so 19 is the floor.

This folder's install carries a development React and its types, for the
package's own typecheck. A host that links the folder rather than installing
the package resolves `react` and `react-dom` to its own single copy (Vite's
`resolve.dedupe`, as `clients/app` does). Its TypeScript treats two
`@types/react` installs as one only when their versions match. The pins here
follow VibeField's, so rosetta's own app (React 19.2.8, types 19.2.18)
typechecks against a second copy of the types: the two agree today, and the
app's typecheck is where a drift would show.
