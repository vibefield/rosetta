import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  computeThumbGeometry,
  createRasterLayout,
  needsMinimap,
  sampleLineIndexes,
  scrollTopForMapOffset,
  scrollTopForThumbDelta,
  wheelDeltaPixels,
} from "../renderer/src/components/file-viewer/minimap/layout.ts";
import {
  MINIMAP_LEFT_GUTTER_DEVICE_PIXELS,
  MINIMAP_MAX_COLUMNS,
  rasterizeMinimap,
} from "../renderer/src/components/file-viewer/minimap/raster.ts";

const TOKEN_COLORS = {
  comment: 2,
  string: 3,
  number: 4,
  keyword: 5,
  type: 6,
  function: 7,
  constant: 8,
  property: 9,
  tag: 10,
  operator: 11,
  plain: 12,
};
const PALETTE = { background: 1, token: TOKEN_COLORS };

function closeTo(actual, expected, epsilon = 0.0001) {
  assert.ok(
    Math.abs(actual - expected) <= epsilon,
    `expected ${actual} to be within ${epsilon} of ${expected}`,
  );
}

function pixel(pixels, width, x, y = 0) {
  return pixels[y * width + x];
}

function raster(lines, width, lineIndexes = null, tabColumns = 2) {
  const rows = lineIndexes === null ? lines.length : lineIndexes.length;
  const height = rows * 2;
  const pixels = new Uint32Array(width * height);
  rasterizeMinimap({
    pixels,
    width,
    height,
    lines,
    lineIndexes,
    tabColumns,
    palette: PALETTE,
  });
  return { pixels, height };
}

describe("file viewer minimap layout", () => {
  it("uses the browser scroll box as the overflow truth", () => {
    assert.equal(needsMinimap(700, 700), false);
    assert.equal(needsMinimap(701, 700), false);
    assert.equal(needsMinimap(724, 700), true);
    assert.equal(needsMinimap(724, 0), false);
  });

  it("uses only the painted rows as an unsampled track", () => {
    const layout = createRasterLayout(100, 700, 2);
    assert.ok(layout);
    assert.equal(layout.rowCount, 100);
    assert.equal(layout.backingHeight, 200);
    assert.equal(layout.cssHeight, 100);
    assert.equal(layout.lineIndexes, null);
  });

  it("bounds a large file to the pane and samples equal source intervals", () => {
    const layout = createRasterLayout(60_000, 700, 2);
    assert.ok(layout);
    assert.equal(layout.rowCount, 700);
    assert.equal(layout.backingHeight, 1_400);
    assert.equal(layout.cssHeight, 700);
    assert.equal(layout.lineIndexes?.length, 700);

    const sample = sampleLineIndexes(10, 3);
    assert.deepEqual([...sample], [1, 4, 7]);
    for (let index = 1; index < layout.lineIndexes.length; index += 1) {
      assert.ok(layout.lineIndexes[index] > layout.lineIndexes[index - 1]);
    }
  });

  it("keeps thumb travel on the painted track in identity mode", () => {
    const top = computeThumbGeometry({
      scrollTop: 0,
      scrollHeight: 1_924,
      clientHeight: 700,
      trackHeight: 100,
    });
    closeTo(top.height, (700 / 1_924) * 100);
    closeTo(top.maxTop, 100 - top.height);
    assert.equal(top.top, 0);

    const bottom = computeThumbGeometry({
      scrollTop: 1_224,
      scrollHeight: 1_924,
      clientHeight: 700,
      trackHeight: 100,
    });
    closeTo(bottom.top, bottom.maxTop);
  });

  it("maps a sampled file continuously even between representative lines", () => {
    const scrollHeight = 60_000 * 19 + 24;
    const maxScroll = scrollHeight - 700;
    const middle = computeThumbGeometry({
      scrollTop: maxScroll / 2,
      scrollHeight,
      clientHeight: 700,
      trackHeight: 700,
    });
    assert.equal(middle.height, 8);
    closeTo(middle.top, middle.maxTop / 2);

    assert.equal(
      scrollTopForMapOffset({ offset: 0, trackHeight: 700, scrollHeight, clientHeight: 700 }),
      0,
    );
    closeTo(
      scrollTopForMapOffset({
        offset: 350,
        trackHeight: 700,
        scrollHeight,
        clientHeight: 700,
      }),
      maxScroll / 2,
    );
    assert.equal(
      scrollTopForMapOffset({
        offset: 700,
        trackHeight: 700,
        scrollHeight,
        clientHeight: 700,
      }),
      maxScroll,
    );
    closeTo(scrollTopForThumbDelta(0, middle.maxTop, middle), maxScroll);
  });

  it("normalizes pixel, line, and page wheel deltas", () => {
    assert.equal(wheelDeltaPixels(3, 0, 19, 700), 3);
    assert.equal(wheelDeltaPixels(3, 1, 19, 700), 57);
    assert.equal(wheelDeltaPixels(1, 2, 19, 700), 700);
  });
});

describe("file viewer minimap raster", () => {
  it("leaves whitespace at the background colour", () => {
    const { pixels } = raster([[{ kind: "plain", text: "     " }]], 16);
    assert.ok(pixels.every((value) => value === PALETTE.background));
  });

  it("advances a tab to the next tab stop", () => {
    const width = 16;
    const { pixels } = raster([[{ kind: "plain", text: "a\tb" }]], width);
    const start = MINIMAP_LEFT_GUTTER_DEVICE_PIXELS;
    assert.equal(pixel(pixels, width, start), TOKEN_COLORS.plain);
    assert.equal(pixel(pixels, width, start + 1), PALETTE.background);
    assert.equal(pixel(pixels, width, start + 2), TOKEN_COLORS.plain);
  });

  it("clips pathological lines at the fixed column budget", () => {
    const width = 110;
    const { pixels } = raster([[{ kind: "plain", text: "x".repeat(500) }]], width);
    const last = MINIMAP_LEFT_GUTTER_DEVICE_PIXELS + MINIMAP_MAX_COLUMNS - 1;
    assert.equal(pixel(pixels, width, last), TOKEN_COLORS.plain);
    assert.equal(pixel(pixels, width, last + 1), PALETTE.background);
  });

  it("keeps adjacent token kinds distinct", () => {
    const width = 16;
    const { pixels } = raster(
      [[{ kind: "keyword", text: "a" }, { kind: "plain", text: "b" }]],
      width,
    );
    const start = MINIMAP_LEFT_GUTTER_DEVICE_PIXELS;
    assert.equal(pixel(pixels, width, start), TOKEN_COLORS.keyword);
    assert.equal(pixel(pixels, width, start + 1), TOKEN_COLORS.plain);
  });

  it("treats Greek as narrow and CJK as full-width", () => {
    const width = 16;
    const lines = [
      [{ kind: "plain", text: "λx" }],
      [{ kind: "plain", text: "界x" }],
    ];
    const { pixels } = raster(lines, width);
    const start = MINIMAP_LEFT_GUTTER_DEVICE_PIXELS;
    assert.equal(pixel(pixels, width, start + 1, 0), TOKEN_COLORS.plain);
    assert.equal(pixel(pixels, width, start + 2, 0), PALETTE.background);
    assert.equal(pixel(pixels, width, start + 1, 2), TOKEN_COLORS.plain);
    assert.equal(pixel(pixels, width, start + 2, 2), TOKEN_COLORS.plain);
  });

  it("uses the supplied source-line sample without allocating ImageData", () => {
    const width = 16;
    const lines = [
      [{ kind: "plain", text: "a" }],
      [{ kind: "keyword", text: "b" }],
      [{ kind: "string", text: "c" }],
    ];
    const { pixels, height } = raster(lines, width, new Uint32Array([2]));
    assert.equal(height, 2);
    assert.equal(
      pixel(pixels, width, MINIMAP_LEFT_GUTTER_DEVICE_PIXELS),
      TOKEN_COLORS.string,
    );
  });
});
