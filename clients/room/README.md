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
| `CatalogPort` | `subscribeMeta` (runtimes, projects, every session row, live: one snapshot then mutations) · `status` → `CatalogStatus` · `projectStats(projectId, window)` → `ProjectStats` (per-day heat and per-session facts) · `usage` · `dayUsage` · `search(CatalogSearch)` · optional `onConnectionChange`, `unavailable` (the host's own words for why the index cannot answer), `boost` and `projectStatsBatch` (every listed project's figures in one ask; present, the room asks it once per refresh instead of `projectStats` per project) | its client's REST and `/ws/meta` (`catalogOver`) |
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

Its host inputs (below), and: `surface` (`"paper"`, the default, paints the room's own ground and plate;
`"host"` paints neither and turns the paper fades into frost over whatever the
host paints) · `portalContainer` (where the dialogs and the search sheet
portal; their sizes are percentages of what holds them) · `edgeInset`,
`scrollFoot`, `floatingFoot` · `onSelect` (the selected session's id, native id,
folder and the host's key when it gave one, on mount and on every change) · `storagePrefix` and `storage` ·
`initialSummaryTab` · `unavailable` and `consent` (the host's words and its
consent screen) · `emptyPrompt` (the empty transcript's sentence in the host's
words; absent, the room's own, below) · `search` (the room's own strip and
sheet, with the host owning the open state; absent, neither is drawn). The ref
handle offers `openAt({ sessionId, timelineId? })` and `stepMatch(delta)`.

**The search keys are the host's (RO-14).** The room binds no key of its own.
The host's window opens the sheet and walks the matches: rosetta's app binds
⌘K (Ctrl K off a Mac) and `/` to the sheet, and ⌘G and ⌘⇧G to the matches. So
the host binds, and the room prints only the keys the host says it binds:

| The host passes | The launcher prints | The empty transcript says |
|---|---|---|
| `search`, no `shortcut` | the room's own label, ⌘K (Ctrl K off a Mac) | "Pick a session — or press ⌘K to search, which is the front door." |
| `search.shortcut: { label }` | `label`, as given | the same sentence with `label` |
| `search.shortcut: null` (it binds no key) | no legend | "Pick a session." |
| no `search` | no launcher | "Pick a session." |
| `emptyPrompt`, beside any row above | as that row says | the host's sentence |

`emptyPrompt` is a prop of the room, not of `search`, so a host that draws no
room search (VibeField, whose search is its own Peek) can still name its own
key there. A host that passes `search` binds ⌘G and ⌘⇧G (Ctrl G and Ctrl Shift
G off a Mac) to the handle's `stepMatch(1)` and `stepMatch(-1)`, because the
match stepper the sheet leaves in the transcript's head names those keys.

## What a host may add (`RoomHostInputs`)

Beyond its ports, a host may hand the room its own colours, marks, sections,
acts, folders and filters (`src/host-inputs.ts`). The room DRAWS them in its
own vocabulary and never interprets them: a host key rides back out
untouched, a host mark is drawn where it is put, a host act is pressed. Every
one is optional, and with none of them the room is exactly what rosetta's app
shows — the reference host passes none. Ids are the room's own
(`ProjectRow.id`, `SessionRow.id`), as the host's catalog handed them in.
The callbacks are called while the room renders, so a host keeps them cheap.

- **`projectTint(projectId)`** — the project's colour, any CSS colour. Absent,
  or null for a project: the room's own dealt tint, which is then the only
  one the room stores.
- **`projectRow(projectId)`** → `{ marks?, line?, live? }` — a ledger row's
  decorations: marks drawn at the start of its path line, a second line
  under the path, and a live count (above zero, the time slot says "n live"
  in the live ink). None is interactive: the row is one button.
- **`projectSections`** (①) and **`sessionSections(projectId)`** (②) →
  `RoomSection[]` — the list in sections, drawn in the order given. A section
  names its rows (`ids`, in the host's order, or `order: "room"` for the
  room's), or names none and takes every row no other section names, in the
  room's order (② with its day heads). A row named twice is drawn in the
  first; when every section names its rows, the rest are not drawn, which is
  how a host filters. `title` draws a head in the list's own break; `fold`
  draws a foot section as one line (its words, its count, its act, such as
  "Show older") that opens in place, and opens by itself when it holds the
  selected row. ②'s head counts what its sections draw.
- **`sessionRow(session)`** → `{ hostKey?, id?, idTitle?, act?, reason? }` — a
  session row's id slot drawn by the host (its mark and the folder, in the
  id's face), its act (`RoomAct`: Resume, Jump to) or the reason where the
  act would be, before the row's time; and the host's own key, which rides
  the selection back out as `RoomSelection.hostKey`, untouched. A row with an
  act is a box under one covering button (the selection, named as the row
  always was) with the act above it, because a button cannot hold another;
  the act is pressed, never a menu, and pressing it leaves the selection.
- **`projectActs(projectId, { folder })`** → `RoomAct[]` — ②'s head acts,
  drawn as the head's own icon buttons after the room's (one may be
  `primary`, on the go fill); `folder` is the folder ③ shows, by the host's
  key (`folders`, below). **`projectCard(projectId)`** — a card under ②'s
  head, above its list. **`sessionActs(session)`** → `{ acts?, reason? }` —
  ④'s head, for the session it names: acts as pills before the activity act,
  or the reason as a caption after the range.
- An act with a `menu` (`RoomMenu`: groups of rows, each with a mark, words,
  a second line, an aside, a check, a step in) opens the room's popover rows
  under it. The rows open on the checked one, the arrow keys walk them, a
  choice or a press outside or Escape closes them; past `filterAfter` rows a
  filter heads them. They listen only while open.
- **`folders(projectId)`** → `RoomFolders` — ③'s head becomes a dropdown of
  the host's folders, by group, each keyed by the host (two machines spell a
  path alike, so the check goes by key, never by path), with its mark in
  front of the name. ③ shows the reader's pick while the session stays, else
  the folder `of(session)` names for the selected session, else `initial`
  (the usual place), else the folder at the room's own root — and tells the
  host which through `onShow(key)`; `projectActs` hears it as `folder`. A
  folder with `elsewhere` is never opened: ③ draws the host's honest state
  in its place. `caption` replaces git's caption while it is shown.
  `WorktreeList` is untouched.
- **`projectFilters`** (①) and **`sessionFilters(projectId)`** (②) →
  `RoomFilter[]` — chips drawn as buttons in the room's chip pill, leading
  the list's bar (① before the rank switch; ② gets a bar only when a project
  has chips), each opening its `RoomMenu`. The host filters, through the
  list's sections, and says what is chosen with the chip's words, `set` and
  a checked row. The room's own search sheet keeps its native selects.

## The reader (`TranscriptReader`)

One session's transcript, read-only, at a line: what a host shows before it
opens the room there (VibeField's Peek). It takes the room's ports and an
address, and draws the room's own document with nothing around it.

```tsx
<TranscriptReader {...ports} at={{ sessionId, timelineId, query }} />
```

- **It lands at `timelineId` and keeps the line marked**, and marks the words
  of `query` in the prose (each word, or a phrase in quotes as one). The
  address is read by value: a new session or line lands again, a re-render
  with the same one does not.
- **It is read-only.** No head band and no acts — no session-work launchers,
  no day menu, no "Latest" pill, no dialog (a long message shows in full) —
  no thread navigation, no key bound, and nothing written back to a room. A
  delegated thread is read inside its root, as the room opens it.
- **It asks only what reading needs:** one `subscribeMeta` while mounted, and
  one `open`/`close` of the session it shows. No status, figures or search.
- **The mark's look is the host's.** Three tokens on any element around the
  reader: `--ah-match` (the wash under a matched word), `--ah-match-weight`,
  and `--ah-match-line` (the matched line's edge, a prompt panel's ring).
  Unset, they are the room's own neutral match — the sheet's
  `--vf-fill-hover` wash at 500, a `--vf-text-tertiary` edge — and never the
  select hue the room's landing ring wears for a moment.

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
