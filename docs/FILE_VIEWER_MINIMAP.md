# File-viewer minimap

**Status:** implemented for `CodeView`; diff support is deliberately deferred.
**Surface:** `clients/app` file viewer.
**Date:** 2026-09-04.

The minimap is a derived, read-only picture of the source listing. It reuses
the tokens `CodeView` already owns, paints a bounded pixel buffer, and treats
the native scroll element as the only scroll truth.

It is not another editor, another highlighter, or another document model.

---

## 1. Product decision

The file viewer is a modal reading detour. Its source files are immutable for
the life of an open, and its body is roughly 700 px high. For this surface the
useful height policy is a full-file map:

- hide the map when the listing does not vertically overflow;
- paint one row per source line while those rows fit;
- midpoint-sample equal source intervals when they do not;
- never scroll the minimap pixels themselves.

This is closer to Monaco's `fit` policy than Sublime's proportional, scrolling
map. The difference is intentional: an immutable viewer does not pay the edit
invalidation cost that drove Sublime's choice, and a reader should be able to
jump from the start to the end of a lockfile from one stable map.

The map is 72 CSS px wide. The Electron window has a 980 px minimum width, so
the default dialog remains about 900–1080 px wide and the map costs roughly
7–8% of it. No phase-one preference or header toggle is required.

---

## 2. Host ground truth

`FileViewerDialog` resolves a registry body and remounts the body on
`handle.key`. Ready source files reach `CodeView`; loading, empty, too-large,
failed, image, binary, and rendered-document states never mount it.

`CodeView` already owns:

| Fact | Value |
|---|---:|
| source row height | 19 CSS px |
| tab stops | every 2 columns |
| virtualizer overscan | 30 rows |
| text read limit | 2 MiB |

It synchronously produces one immutable `TokenLine[]` with `highlight()`, then
uses the same array for its visible virtual rows and the minimap. Markdown
`Source` and SVG `Source` already render `CodeView`, so they inherit the map
without special cases.

The scroll element is `.ah-fileview__scroll.ah-code`. It has native
`overflow: auto`, 8 px top padding, 16 px bottom padding, and may have a
horizontal scrollbar. Its `scrollTop`, `scrollHeight`, and `clientHeight` are
therefore authoritative. Line-count arithmetic is not a substitute for its
geometry.

---

## 3. Architecture and files

Everything remains in the file-viewer package:

```
clients/app/renderer/src/components/file-viewer/
  views/CodeView.tsx
  minimap/
    CodeMinimap.tsx   # DOM lifecycle, observers, pointer and wheel wiring
    layout.ts         # pure raster, thumb, click, drag, and wheel geometry
    raster.ts         # pure Uint32 pixel writer
    palette.ts        # live CSS colours -> packed, composited pixels
```

Tests are in:

```
clients/app/test/file-viewer-minimap.test.mjs
```

There is one opaque text canvas and one DOM viewport thumb. A second decoration
canvas is not useful for an immutable source view; add a layer only when a
dynamic decoration has a real consumer.

No dependency is added. In particular, do not add Monaco, CodeMirror, a
Unicode-width package, WebGL, or a canvas test shim.

---

## 4. Layout

`CodeView` gains one flex shell:

```
.ah-code-shell
  .ah-fileview__scroll.ah-code
    .ah-code__sizer
  .ah-code-minimap
    .ah-code-minimap__stage
      canvas.ah-code-minimap__text
      div.ah-code-minimap__slider
```

The host stretches to the scroller height and reserves 72 px of width. The
stage is only as tall as the painted map. This distinction matters for an
unsampled medium file: at 2× density, 100 source lines occupy 100 CSS px, not
the entire 700 px host. The thumb must never travel through the blank ground
below those pixels.

The host is absent when hidden, so the source listing recovers all 72 px.

### Visibility

The map is visible when:

```
clientHeight > 0 && scrollHeight - clientHeight > 1
```

The one-pixel tolerance avoids fractional-layout noise. A `ResizeObserver`
watches the scroller and its sizer while the minimap host is absent, so a
resize can create or remove the host without a measurement deadlock.

---

## 5. Raster height and sampling

Constants are physical backing-store pixels:

| Name | Value |
|---|---:|
| character width | 1 |
| character height | 2 |
| row height | 2 |
| left gutter | 2 |
| maximum columns | 96 |
| maximum render scale | 2× |

Render scale is:

```
scale = min(2, max(1, devicePixelRatio))
```

Capping at 2× keeps one ImageData plus the canvas backing store bounded while
remaining native-resolution on the app's normal Retina target.

Given host height `H` in CSS pixels and `N` source lines:

```
availableDeviceHeight = floor(H * scale)
capacity              = floor(availableDeviceHeight / 2)
paintedRows           = min(N, capacity)
backingHeight         = paintedRows * 2
trackCssHeight        = backingHeight / scale
```

If `N <= capacity`, raster row `i` paints source line `i`.

If `N > capacity`, each raster row represents an equal source interval. Its
midpoint is the representative line:

```
from(i)   = floor(i       * N / paintedRows)
to(i)     = floor((i + 1) * N / paintedRows) - 1
sample(i) = floor((from(i) + max(from(i), to(i))) / 2)
```

The map is built in `O(paintedRows × maxColumns)`, not `O(fileBytes)`. A
60,000-line file in the default pane still paints at most about 700 rows and
96 columns per row.

Sampling chooses pixels only. It never defines navigation or viewport
geometry. A representative source line may not intersect the current viewport,
and that must not make the thumb disappear.

---

## 6. Raster

`rasterizeMinimap` accepts a `Uint32Array`, dimensions, `TokenLine[]`, the
optional sample map, tab columns, and a packed palette. It does not construct
`ImageData` and has no DOM dependency. `CodeMinimap` owns the browser object;
node tests can exercise the exact renderer with an ordinary typed array.

For each painted line:

1. Start after the two-device-pixel gutter.
2. Walk the existing tokens and Unicode code points.
3. Flush a coloured rectangle around each non-whitespace run.
4. Advance a tab to the next tab stop:

   ```
   advance = tabColumns - (column % tabColumns)
   ```

5. Stop at 96 columns or the physical canvas width.

Consecutive pixels in a run are filled with `Uint32Array.fill`. The buffer is
prefilled with the opaque file-viewer background, and the component performs
one `putImageData` per paint.

The compact width predicate covers East Asian wide ranges and leaves Greek,
Cyrillic, Hebrew, and normal symbols at one column. Combining marks and the
zero-width joiner add no column. This is enough for a 72 px silhouette without
shipping a Unicode-width database.

---

## 7. Palette

The minimap consumes the same roles as the listing:

```
comment    --vf-text-faint
operator   --vf-text-tertiary
plain      --vf-text-secondary
keyword    --ah-syn-keyword
tag        --ah-syn-keyword
type       --ah-syn-type
function   --ah-syn-function
string     --ah-syn-string
number     --ah-syn-value
constant   --ah-syn-value
property   --ah-syn-property
background --vf-chrome-solid
```

An off-layout probe inside the live minimap resolves each custom property with
`getComputedStyle`. A 1×1 scratch canvas then:

1. fills the opaque background;
2. draws the resolved foreground with `globalAlpha = 0.58`;
3. reads the resulting RGBA pixel;
4. packs it for the platform's byte order.

The scratch-canvas step is deliberate. The neutral text roles differ partly
by alpha, and modern Chromium may preserve CSS Color 4 syntax such as
`oklab()` in computed values. Browser compositing preserves both correctly;
hand parsing does not.

Changing `class` or `data-theme` on `<html>` resolves the palette again and
repaints. Scroll never resolves colours.

---

## 8. Scroll geometry

Let:

```
trackH    = trackCssHeight
viewportH = scroller.clientHeight
contentH  = scroller.scrollHeight
maxScroll = max(0, contentH - viewportH)
```

The DOM thumb is a scrollbar projection over the painted track:

```
thumbH   = clamp(trackH * viewportH / contentH, 8, trackH)
maxTop   = max(0, trackH - thumbH)
thumbTop = maxScroll > 0 ? scrollTop / maxScroll * maxTop : 0
```

This formula is identical in identity and sampled modes. It includes source
padding and horizontal-scrollbar effects because it uses the browser's real
scroll metrics.

The scroll listener coalesces work with `requestAnimationFrame` and changes
only the thumb's `transform: translate3d(...)`. Thumb height changes only when
layout changes. Scroller updates remain authoritative during a drag; there is
no competing optimistic thumb position.

### Click

A primary pointer press outside the thumb warps the corresponding source
position into the centre of the viewport:

```
fraction = clamp(pointerY / trackH, 0, 1)
target   = clamp(fraction * contentH - viewportH / 2, 0, maxScroll)
```

The pointer coordinate is corrected by `trackH / getBoundingClientRect().height`
so the dialog's opening transform cannot skew an early interaction. A map
click does not capture subsequent pointer movement.

### Drag

The thumb captures the pointer. At pointer-down, snapshot its geometry:

```
scrollPerTrackPixel = maxTop > 0 ? maxScroll / maxTop : 0
target = clamp(startScroll + deltaY * scrollPerTrackPixel, 0, maxScroll)
```

### Wheel

Wheel deltas are forwarded to the scroller. Pixel, line, and page delta modes
are normalized with the 19 px code line height and current viewport height.
The non-passive host listener prevents the page behind the modal from taking
the same wheel gesture.

---

## 9. Lifecycle and invalidation

The immutable source model keeps invalidation small:

| Event | Work |
|---|---|
| new `CodeView` / new `lines` identity | rebuild sample map and raster |
| host or scroller resize | rebuild layout and raster |
| theme mutation | resolve palette and rebuild raster |
| display-density change | resize backing store and rebuild raster |
| source scroll | update DOM thumb only |

`ResizeObserver` watches the host and scroller. A resolution media query is
re-armed after each density change; window resize is a fallback. Canvas work
is coalesced into one animation frame.

If Canvas 2D or palette resolution is unavailable, the component removes the
map and leaves the native viewer intact.

---

## 10. Accessibility

The minimap is supplementary spatial memory, not the only scrollbar:

- `.ah-code-minimap` is `aria-hidden="true"` and `role="presentation"`;
- it has no tab stop and exposes no duplicate source text;
- the native overflow element and its visible scrollbar remain in place;
- keyboard and assistive-technology scrolling continue to use that element;
- reduced motion removes the thumb opacity transition.

If a later change hides the native scrollbar, it must separately establish
the keyboard and ARIA contract for a custom scrollbar. This implementation
does not make that trade.

---

## 11. Tests and performance contract

Pure node tests cover:

- fit versus sampled raster height;
- the short identity-map track, not the full host height;
- 60,000-line bounded sampling and monotonic representatives;
- continuous sampled thumb geometry where no sample intersects the viewport;
- top, middle, and bottom click mapping;
- drag as the inverse of thumb travel;
- pixel, line, and page wheel units;
- whitespace, tab stops, 96-column clipping, token colours, and sampled rows;
- narrow Greek versus full-width CJK.

Browser acceptance covers:

- overflowing source shows a 72 px map; fitting source recovers the width;
- Markdown and SVG source modes inherit it;
- rendered Markdown, images, binary/hex, empty, failed, loading, and too-large
  states do not show it;
- light and dark use the live syntax palette;
- click, drag, wheel, native scroll, and window resize stay aligned;
- scrolling does not call `putImageData`.

At the default pane and the 2× render cap, one 72×700 CSS px ImageData is about
0.77 MiB; the canvas backing store brings peak pixel memory to about 1.54 MiB.
The target for the bounded raster and its one `putImageData` remains under
8 ms in the packaged Electron renderer. Measure total file-open time
separately: tokenization and `widestLine` are existing costs, not minimap work.

---

## 12. Delivery plan

### Landed together: source minimap

The first shippable unit includes all correctness paths:

1. flex shell and hide-when-fits behavior;
2. identity and sampled raster;
3. continuous thumb, click, drag, and wheel geometry;
4. live light/dark palette and density invalidation;
5. pure layout/raster tests and browser smoke verification.

Sampling is not a later enhancement. The existing 2 MiB contract permits
files far beyond the pane capacity, so an identity-only release would need to
hide precisely the large files the map is meant to help with.

### Deferred: `DiffBody`

Diff support is a separate change. Its model includes hunk, add, delete,
context, and note rows, and its sticky diffstat sits before the virtualized
sizer. When it is implemented:

- extract a small row-source adapter rather than forcing diff rows into
  `TokenLine[]`;
- sample token silhouettes with the same midpoint policy;
- bucket every add/delete/hunk marker into output rows so a narrow hunk is not
  lost merely because its text row was not sampled;
- paint immutable diff markers in the same canvas;
- use `--vf-green` and `--vf-red` only for add/delete gutters;
- use one 56 px map in the commit viewer's right pane.

Do not add a second canvas until a dynamic decoration independently requires
invalidation.
