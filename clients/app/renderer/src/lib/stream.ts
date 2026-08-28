// The models streamgraph's geometry. Byron & Wattenberg's wiggle-minimising
// baseline (the one d3 implements as stackOffsetWiggle) spreads distortion
// across the layers instead of dumping it on the top one — which is what
// makes a streamgraph readable, and also what costs it a zero.

type Point = [number, number];

/** Catmull-Rom through the points, expressed as beziers — the river reads as
 * a river without inventing values between days. */
export function smooth(pts: Point[]): string {
  if (pts.length < 2) return "";
  let d = `M${pts[0][0].toFixed(1)} ${pts[0][1].toFixed(1)}`;
  for (let i = 0; i < pts.length - 1; i += 1) {
    const p0 = pts[i > 0 ? i - 1 : 0];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    d +=
      `C${(p1[0] + (p2[0] - p0[0]) / 6).toFixed(1)} ${(p1[1] + (p2[1] - p0[1]) / 6).toFixed(1)},` +
      `${(p2[0] - (p3[0] - p1[0]) / 6).toFixed(1)} ${(p2[1] - (p3[1] - p1[1]) / 6).toFixed(1)},` +
      `${p2[0].toFixed(1)} ${p2[1].toFixed(1)}`;
  }
  return d;
}

export function wiggleBaseline(layers: number[][]): number[] {
  const m = layers.length;
  const n = layers[0].length;
  const y0 = [0];
  for (let j = 1; j < n; j += 1) {
    let s1 = 0;
    let s2 = 0;
    for (let i = 0; i < m; i += 1) {
      const v = layers[i][j];
      let s3 = (v - layers[i][j - 1]) / 2;
      for (let k = 0; k < i; k += 1) s3 += layers[k][j] - layers[k][j - 1];
      s1 += v;
      s2 += s3 * v;
    }
    y0.push(y0[j - 1] - (s1 ? s2 / s1 : 0));
  }
  return y0;
}

/** One `<path d>` per layer, bottom-up, into a W×H viewBox. */
export function streamPaths(layers: number[][], W: number, H: number): string[] {
  const n = layers[0]?.length ?? 0;
  if (n < 2 || layers.length === 0) return [];
  const base = wiggleBaseline(layers);
  const bounds = [base.slice()];
  for (let i = 0; i < layers.length; i += 1) {
    bounds.push(bounds[i].map((v, j) => v + layers[i][j]));
  }
  let lo = Infinity;
  let hi = -Infinity;
  for (const b of bounds) {
    for (const v of b) {
      if (v < lo) lo = v;
      if (v > hi) hi = v;
    }
  }
  const span = hi - lo || 1;
  const x = (j: number) => (j / (n - 1)) * W;
  const y = (v: number) => H - ((v - lo) / span) * (H - 4) - 2;

  return layers.map((_, li) => {
    const top: Point[] = bounds[li + 1].map((v, j) => [x(j), y(v)]);
    const bot: Point[] = bounds[li].map((v, j): Point => [x(j), y(v)]).reverse();
    const botPath = smooth(bot);
    return `${smooth(top)} L${bot[0][0].toFixed(1)} ${bot[0][1].toFixed(1)} ${botPath.slice(
      botPath.indexOf("C"),
    )} Z`;
  });
}
