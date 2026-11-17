# @vibefield/rosetta-room

rosetta's History room as a package: the projects, sessions and worktree
columns, the summary panels and charts, and the transcript with its folds,
behind injected ports. The room dials nothing. A host hands it a catalog, a
way to open one session's body, an asset source and, on a desktop, a host
port; each host writes its own adapters. rosetta's own app
(`clients/app/renderer/src/host`) is the reference host.

```sh
npm install @vibefield/rosetta-room react@19 react-dom@19
```

```tsx
import { HistoryRoom } from "@vibefield/rosetta-room";
import "@vibefield/rosetta-room/styles/room.css"; // after the host's --vf-* tokens

<HistoryRoom catalog={catalog} sessions={sessions} assets={assets} host={host} />;
```

`@vibefield/rosetta-room/fixtures` renders the room with no daemon at all:
`<HistoryRoom {...createDemoPorts()} />` (six projects, eleven sessions, a year
of figures, two transcripts with images, web lines and two sent videos (one
captured, one not), all relative to now). The demo commits no encoded video: a
video member reads as a header-only MP4, so pressing play ends at "Could not
play video".
`createColdImport({ now })` returns `{ ports, consent, daemon, controller }`
for a first import instead, over a seeded synthetic archive (by default the
MacBook trace's shape: 8,612 sessions in 1,139 trees over 119 projects;
`scale` shrinks it). It owns no timer: nothing happens until the controller
steps simulated time, and each step delivers the flips, resyncs and status
rosettad would have by then.

```tsx
const sim = createColdImport({ now: Date.now(), consent: "granted" });
<HistoryRoom {...sim.ports} />;
sim.controller.advanceToPhase("metadata"); // or advanceTo(ms)
```

A fresh import waits for consent unless told it was `granted`: pass
`sim.consent` (the roll call's `discoveries` and `enableRuntimes`) to a
consent screen, or call `sim.consent.enableRuntimes([...])`. `course:
"resumed"` replays a cross-launch resume instead, which opens nothing until it
publishes. `sim.daemon.fetch` answers the daemon's REST routes for a host that
builds its ports over a `RosettaClient` (`fetchFn`); `/ws/meta` and the session
sockets stay the simulator's.

## The ports (`src/ports.ts`)

| Port | What the room asks | rosetta's app answers with |
|---|---|---|
| `CatalogPort` | `subscribeMeta` (runtimes, projects, every session row, live: one snapshot then mutations) · `status` → `CatalogStatus` · `projectStats(projectId, window)` → `ProjectStats` (per-day heat and per-session facts) · `usage` · `dayUsage` · `search(CatalogSearch)` · optional `onConnectionChange`, `unavailable` (the host's own words for why the index cannot answer), `boost` and `projectStatsBatch` (every listed project's figures in one ask; present, the room asks it once per refresh instead of `projectStats` per project) | its client's REST and `/ws/meta` (`catalogOver`) |
| `SessionPort` | `open(sessionId)` → a `SessionClient` bound to that session (its live subscription, child threads and team members each open their own; entries, report work, commands, plans, file targets, stored members), or a promise of one; `close(sessionId)` | the one client for every session; close is nothing |
| `AssetSource` | `imageAssetUrl(asset, variant)` → a URL, null, or a promise of either · `attachmentAssetUrl(asset)` → the attachment's address, for identity and can-open only · optional `mediaAssetUrl(asset)` → an address a `<video src>` can load, null, or a promise of either | the daemon's own URLs |
| `HostPort` (optional) | `homeDir` · `platform` · `openInBrowser(name, bytes, browserId?)` · optional `browsers()` · `favicon(site)` · optional `worktree: { open, gitFacts, fileAction, ports }` | the Electron bridge (`hostPortOver`) |

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
- **A transcript image is drawn only through `imageAssetUrl`.** It is a
  recorded original (one member, PNG, JPEG, GIF, WebP, BMP or ICO, with no
  `image_variants`) or, in a projection built before deferred renditions, a
  complete legacy WebP bundle. One transcript may hold both; anything else
  keeps its placeholder. Every rendition, `full` (1:1) included, is a WebP the
  daemon encoded: rosetta's SDK answers an original with
  `?image=<variant>&v=1` and a bundle with its own members. A host that
  fetches the bytes fetches that address, never the member's plain URL, which
  returns the recorded bytes for opening or saving only. An asset-backed
  timeline artifact that is a recorded image opens the same way. A tile is
  `aria-busy`, in its loading style, until its rendition draws. Rooms
  published up to 0.1.2 accept bundles only, and show a newer daemon's
  originals as placeholders. The SDK published as 0.1.2 answers an
  original's `full` with its plain URL, so a host pairs this room with the
  SDK released beside it.
- **A sent video plays in place, and only when asked.** A tool row's
  attachment of kind `video` is a recorded member (`/api/assets/<digest>.mp4`
  or `.mov`, `video/mp4` or `video/quicktime`, no `image_variants`) or a
  reference to a file that was sent but not captured. The rail draws a
  captured one as a tile with a play glyph, the file's name and its size, at
  the rail's height and the video's own shape (16:9 when its dimensions are
  unknown, never narrower than a square nor wider than 21:9). There is no
  poster and no thumbnail: the room asks nothing for a video until the reader
  presses play. Then it asks the optional **`mediaAssetUrl(asset)`** for an
  address a `<video src>` can load. A host answers one only for an address
  that honours byte ranges: served whole, without ranges, a video plays but
  cannot seek, and no error tells the room to fall back. The daemon serves a
  member whole, so rosetta's app leaves `mediaAssetUrl` out. A host without
  it, one that answers null, or one whose address the element cannot load
  needs nothing more: the room reads the member's bytes through the reading
  session's `readAsset` and plays an object URL it makes and revokes itself,
  which seeks. If the bytes cannot be read or played either,
  the tile says "Could not play video" and offers Retry, which asks the host
  again. A video that was not captured keeps the tile's shape, says "Video not
  captured" with its name and size, and is not a button; an image sent the
  same way says "Image not captured". A video is never passed to
  `imageAssetUrl`, never opens the image viewer and is not in its gallery.
  While a video has focus the rail's arrow, Home and End keys are the
  video's. `attachmentAssetUrl` is still asked for a video, as its identity.
- **`boost` is optional** because a grant never opens a write: only a host
  holding the daemon's bearer offers it. Without it the index keeps its order.
- **`browsers()`** returns installed browser ids, names and icon URLs. HTML
  and SVG viewers show one launch icon per browser, refreshing when opened or
  the window regains focus. The host owns executable discovery and launch;
  the room sends back only an id and the exact displayed bytes. Hosts without
  discovery retain their existing default-browser action.
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
`initialSummaryTab` · `unavailable`, `unavailableTitle` and `connectingTitle`
(the disconnected face, below) · `consent` (the host's consent screen) ·
`emptyPrompt` (the empty transcript's sentence in the host's words; absent,
the room's own, below) · `search` (the room's own strip and
sheet, with the host owning the open state; absent, neither is drawn). The ref
handle offers `openAt({ sessionId, timelineId? })`, `stepMatch(delta)` and
`refreshStatus()` (ask the index's status now, for a host that just moved its
state itself — rosetta's roll call calls it once consent is granted).

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

**While full text cannot answer, the sheet searches titles.** Until the
catalog's status says search can answer (`canonicalReady` false, or search
readiness `building` or `failed`, as through a first import and the index
build after it), the sheet matches the query against the session titles the
room already holds, under "Titles that match", and the launcher counts those
titles ("Search 1,174 conversation titles"). The line under the matches says
what full text waits on: "Full-text search arrives when the import finishes."
before publication, "…when its index is built." after it, or "Full-text
search is unavailable." The foot reads "Titles only until the import
finishes", then "Full-text search is being built · 37% · showing title
matches" (the % only from the index's own document count), or "Full-text
search unavailable · showing title matches". A title row the running import
has not reached reads "Not imported yet". Opening one opens its thread as a
hit does, with no message to land on. When full text starts answering, the
room asks the query again, and the titles stay until that answer arrives. A
catching-up, stale or ready index keeps the ordinary sheet. The foot's meter
fills only from the index's document count, sweeps for work with no count,
and is absent while the import is paused or the index has failed.

**The disconnected face can be all the host's words.** While the catalog has
delivered no rows, the room draws one face: a title, a sentence under it, and,
while it is still reaching the index, a sweeping meter (reaching has no amount,
so none is drawn; once the index has stopped answering there is no meter at
all). Every part a host leaves out stays the room's own, so rosetta's app
reads as it always has; a host that never names its daemon to a person gives
both titles and its sentence.

| While | The title |
|---|---|
| the room is still reaching the index | `connectingTitle`, else "Connecting to rosettad" |
| the index has stopped answering | `unavailableTitle`, else "rosettad is not answering" |

The sentence under either title is the `unavailable` prop until a `status`
ask fails; from then on it is the catalog's `unavailable()`, else that ask's
own error. The reader takes `unavailableTitle` and `unavailable` too (below).

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
  pressing the act (or opening its rows) leaves the selection.
- **`projectActs(projectId, { folder })`** → `RoomAct[]` — ②'s head acts,
  drawn as the head's own icon buttons after the room's (one may be
  `primary`, on the go fill); `folder` is the folder ③ shows, by the host's
  key (`folders`, below). **`projectCard(projectId)`** — a card under ②'s
  head, above its list. **`sessionActs(session)`** → `{ acts?, reason? }` —
  ④'s head, for the session it names: acts as pills before the activity act,
  or the reason as a caption after the range.
- An act with a `menu` (`RoomMenu`: groups of rows, each with a mark, words,
  a second line, an aside, a check, a step in) opens the room's popover rows
  from it. They are drawn above the columns, in the room's root, anchored to
  their control, flipped above it when there is more room there, and held
  inside the room's box (no column clips them). Opened by a key they land on
  the checked row, which wears the ring; opened by a pointer they take focus
  with no ring until a key walks them. The arrow keys walk them, a choice or a
  press outside or Escape closes them; past `filterAfter` rows a filter heads
  them. They listen only while open (and to the window's resize and scroll,
  to stay on their control).
- **`folders(projectId)`** → `RoomFolders` — ③'s head becomes a dropdown of
  the host's folders, by group, each keyed by the host (two machines spell a
  path alike, so the check goes by key, never by path), with its mark in
  front of the name. ③ shows the reader's pick while the session stays, else
  the folder `of(session)` names for the selected session, else `initial`
  (the usual place), else the folder at the room's own root — and tells the
  host which through `onShow(key)`; `projectActs` hears it as `folder`. A
  folder with `elsewhere` is never opened: ③ draws the host's honest state
  in its place. `caption` replaces git's caption while it is shown.
  `WorktreeList` is untouched. A folder's `act` (H4: "Not part of this
  project") sits beside its row in the list, pressed, never a menu; → from
  the row reaches it, ← comes back, and the list closes after it.
- **`projectRow(projectId).act`** (H4) — an act at a ledger row's end,
  before its time (pin, hide), or its rows. The row becomes the same box as
  a session row with an act: one covering button (the selection), the act
  above it.
- **`projectRename(projectId)`** → `{ onRename(name), label? }` (H4) — ②'s
  head name is the control: pressed, it becomes a field holding the name.
  Enter commits the trimmed, changed, non-empty name; Escape or leaving the
  field cancels. A rejected `onRename` keeps the field open and says why
  under it ("Could not rename — <the error's words>"). The head names the
  project as the host's catalog last said, until it says otherwise.
- **`projectFilters`** (①) and **`sessionFilters(projectId)`** (②) →
  `RoomFilter[]` — chips drawn as buttons in the room's chip pill, leading
  the list's bar (① before the rank switch; ② gets a bar only when a project
  has chips), each opening its `RoomMenu`. The host filters, through the
  list's sections, and says what is chosen with the chip's words, `set` and
  a checked row. The room's own search sheet keeps its native selects.
- **`repositoryUrl(projectId)`** — the address ②'s repository act opens.
  Undefined: the room's own reading of `git_repo`; null: no act.
- **`waitMark`** → `RoomWaitMark` — the host's mark for the room's
  waits: `mark(place, { resting })` draws it for a place (`"view"`,
  `"column"`, `"card"`), at rest while the work it stands for is paused, and
  `carried` says the host's own hold already stands where the room's first
  one comes. With it:
  - **the view's waits are one hold** — reaching the index, then the cold
    start before its first project: the mark at the centre of the room's box,
    one line 26 under it (`connectingTitle`, or the step's own title with the
    ellipsis of work under way) and its count 4 under that ("1,333 files
    checked"). It is late (nothing for 120ms), stands 800ms once it has shown,
    fades over 280ms (120ms under reduced motion), and what comes next waits
    for it; a new line crossfades in place. An index that has stopped
    answering keeps the room's face: a failure is no wait.
  - **the import card wears the mark** where IMPORTING HISTORY stood, the step
    beside it; its words keep to the step's title and its count, a fraction
    the step knows is a colourless 2px hairline (none for one it does not),
    and the detail, the amount, the connection and the timer fold under Agent
    details. Paused updates stand the mark at rest.
  - **a conversation's wait is a hold in its column**, its sketch and sweep
    gone; a load that failed keeps its own face and Try again.
  - **nothing sweeps beside the mark**: a meter that knows no amount draws
    nothing (a veil keeps its words), and one that knows still fills.

  Absent: every wait is the room's own, as rosetta's app shows it.

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
- **Its not-answering face takes the host's words**, as the room's does:
  `unavailableTitle` (else "The index is not answering") and, under it, the
  catalog's `unavailable()`, else the `unavailable` prop. Its opening face,
  "Opening the session", names no process and stays its own.
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
`--ah-floating-foot`, `--ah-inset-*`, and the reader's match look
`--ah-match`, `--ah-match-weight`, `--ah-match-line`, read with neutral
fallbacks so a host sets them on any element around the room), `--tr-*`, and
the project tint band `--tint-l`/`--tint-c`. What a host adds is styled in
`styles/host-inputs.css`, whose every rule matches only an element drawn for
a host input. The root carries `data-ah-surface`, and
`data-ah-overlay` while one of its dialogs or the sheet is open, so a host can
stop its own chrome (a drag region, say) under it.

## Building against it

The npm package ships ESM JavaScript, TypeScript declarations, and its stylesheets.
Its three exports are the room, fixtures, and `styles/room.css`. React and React DOM
are peers at `>=19 <20`; the host supplies one copy of each. Mille and Mille UI are
registry dependencies, so installing Rosetta needs no sibling checkout.

For development, run `npm ci` at the repository root, then `npm run build:packages`.
`npm run typecheck -w @vibefield/rosetta-room` checks the room under `strict`,
`exactOptionalPropertyTypes`, `noUncheckedIndexedAccess`, and `verbatimModuleSyntax`.
The workspace app aliases the room's source for live development and deduplicates
React. `npm run check:packages` verifies the packed exports, declarations, and CSS
in an independent consumer before release.
