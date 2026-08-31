// The registry's way in to the diff pane.
//
// A `ViewerDefinition` renders one `OpenFile`; `DiffBody` renders one
// unified diff. Keeping them apart is what lets the commit viewer lay two
// panes out without pretending its selection is a file the registry
// resolved.

import { DiffBody } from "./DiffBody";
import type { ViewBodyProps } from "../types";

export function DiffView({ file }: ViewBodyProps) {
  return <DiffBody unified={file.unified ?? ""} name={file.handle.name} />;
}
