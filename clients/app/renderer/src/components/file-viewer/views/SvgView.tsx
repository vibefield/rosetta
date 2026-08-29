// SVG is the one format that is honestly both a picture and a document, so it
// gets both readings rather than a guess about which one you wanted.
//
// The rendered half goes through `<img>`, which is the sandbox: an SVG loaded
// as an image cannot run its own scripts, load external references, or reach
// the page around it. Inlining the markup would hand a file in someone else's
// repository the run of this renderer.

import type { ViewBodyProps } from "../types";
import { CodeView } from "./CodeView";
import { ImageView } from "./ImageView";

export const SVG_MODES = [
  { id: "rendered", label: "Rendered" },
  { id: "source", label: "Source" },
] as const;

export function SvgView(props: ViewBodyProps) {
  return props.mode === "source" ? <CodeView {...props} /> : <ImageView {...props} />;
}
