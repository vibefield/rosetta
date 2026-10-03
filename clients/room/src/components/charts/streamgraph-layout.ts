type Point = readonly [x: number, y: number];

export interface StreamgraphLayerInput {
  id: string;
  values: readonly number[];
}

export interface StreamgraphLayerPath {
  seriesId: string;
  path: string;
  /** Sample centers use the same stacked boundaries as the rendered band. */
  samples: readonly Point[];
}

export interface StreamgraphLayoutOptions {
  width: number;
  height: number;
  verticalPadding?: number;
}

function finiteMagnitude(value: number | undefined): number {
  return Number.isFinite(value) ? Math.max(0, value ?? 0) : 0;
}

/**
 * Catmull-Rom interpolation expressed as cubic Bézier commands. The curve
 * passes through every supplied point, so smoothing does not discard samples.
 */
function createSmoothCurve(points: readonly Point[], initialCommand: "M" | "L"): string {
  if (points.length === 0) return "";
  // Every point read here exists: the first is guarded above, and the loop
  // runs while index + 1 < points.length, clamping the previous point at 0.
  const pointAt = (index: number): Point => points[index]!;

  let path = `${initialCommand}${pointAt(0)[0].toFixed(1)} ${pointAt(0)[1].toFixed(1)}`;
  for (let index = 0; index < points.length - 1; index += 1) {
    const previous = pointAt(index > 0 ? index - 1 : 0);
    const current = pointAt(index);
    const next = pointAt(index + 1);
    const following = points[index + 2] ?? next;

    path +=
      `C${(current[0] + (next[0] - previous[0]) / 6).toFixed(1)} ` +
      `${(current[1] + (next[1] - previous[1]) / 6).toFixed(1)},` +
      `${(next[0] - (following[0] - current[0]) / 6).toFixed(1)} ` +
      `${(next[1] - (following[1] - current[1]) / 6).toFixed(1)},` +
      `${next[0].toFixed(1)} ${next[1].toFixed(1)}`;
  }
  return path;
}

/** Byron and Wattenberg's wiggle-minimising streamgraph baseline. */
function calculateWiggleBaseline(layers: readonly (readonly number[])[]): number[] {
  const sampleCount = layers[0]?.length ?? 0;
  if (sampleCount === 0) return [];
  // Every read here exists: each loop stays below its own bound, every layer
  // holds sampleCount samples (the layout builds them at one length),
  // sampleIndex starts at 1, and the baseline has sampleIndex entries by the
  // time it is read.
  const layerAt = (index: number): readonly number[] => layers[index]!;
  const sampleAt = (values: readonly number[], index: number): number => values[index]!;

  const baseline = [0];
  for (let sampleIndex = 1; sampleIndex < sampleCount; sampleIndex += 1) {
    let totalAtSample = 0;
    let weightedSlope = 0;

    for (let layerIndex = 0; layerIndex < layers.length; layerIndex += 1) {
      const layer = layerAt(layerIndex);
      const value = sampleAt(layer, sampleIndex);
      let slope = (value - sampleAt(layer, sampleIndex - 1)) / 2;

      for (let earlierLayerIndex = 0; earlierLayerIndex < layerIndex; earlierLayerIndex += 1) {
        const earlierLayer = layerAt(earlierLayerIndex);
        slope += sampleAt(earlierLayer, sampleIndex) - sampleAt(earlierLayer, sampleIndex - 1);
      }

      totalAtSample += value;
      weightedSlope += slope * value;
    }

    baseline.push(
      sampleAt(baseline, sampleIndex - 1) -
        (totalAtSample > 0 ? weightedSlope / totalAtSample : 0),
    );
  }
  return baseline;
}

/**
 * Builds one closed SVG path per series, preserving the order of `layers`.
 * Values are normalised defensively so malformed runtime data cannot produce
 * `NaN` path commands.
 */
export function createStreamgraphLayout(
  layers: readonly StreamgraphLayerInput[],
  sampleCount: number,
  { width, height, verticalPadding = 2 }: StreamgraphLayoutOptions,
): StreamgraphLayerPath[] {
  const safeSampleCount = Number.isFinite(sampleCount)
    ? Math.max(0, Math.floor(sampleCount))
    : 0;
  if (
    layers.length === 0 ||
    safeSampleCount < 2 ||
    !Number.isFinite(width) ||
    !Number.isFinite(height) ||
    width <= 0 ||
    height <= 0
  ) {
    return [];
  }

  const valuesByLayer = layers.map((layer) =>
    Array.from({ length: safeSampleCount }, (_, index) => finiteMagnitude(layer.values[index])),
  );
  const baseline = calculateWiggleBaseline(valuesByLayer);
  const boundaries: number[][] = [baseline];

  for (const values of valuesByLayer) {
    const previousBoundary = boundaries[boundaries.length - 1]!;
    boundaries.push(previousBoundary.map((value, index) => value + values[index]!));
  }

  let minimum = Number.POSITIVE_INFINITY;
  let maximum = Number.NEGATIVE_INFINITY;
  for (const boundary of boundaries) {
    for (const value of boundary) {
      minimum = Math.min(minimum, value);
      maximum = Math.max(maximum, value);
    }
  }
  const span = maximum - minimum || 1;
  const safeVerticalPadding = Number.isFinite(verticalPadding) ? verticalPadding : 0;
  const padding = Math.min(Math.max(0, safeVerticalPadding), height / 2);
  const drawableHeight = Math.max(0, height - padding * 2);
  const xPosition = (index: number) => (index / (safeSampleCount - 1)) * width;
  const yPosition = (value: number) =>
    height - ((value - minimum) / span) * drawableHeight - padding;

  return layers.map((layer, layerIndex) => {
    const upperPoints: Point[] = boundaries[layerIndex + 1]!.map((value, sampleIndex) => [
      xPosition(sampleIndex),
      yPosition(value),
    ]);
    const lowerPoints: Point[] = boundaries[layerIndex]!
      .map(
        (value, sampleIndex): Point => [xPosition(sampleIndex), yPosition(value)],
      )
      .reverse();

    return {
      seriesId: layer.id,
      path: `${createSmoothCurve(upperPoints, "M")} ${createSmoothCurve(lowerPoints, "L")} Z`,
      samples: upperPoints.map(([x, upperY], index) => [
        x, (upperY + yPosition(boundaries[layerIndex]![index]!)) / 2,
      ] as const),
    };
  });
}
