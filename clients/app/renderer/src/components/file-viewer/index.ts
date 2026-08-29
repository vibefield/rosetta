// The file viewer: a popup that shows one file, with a view per kind of file.
//
// It depends on nothing above it. Give it a name, a size and a `FileSource`,
// and it reads and draws the file — which is why the worktree column owns the
// engine and this owns none of it.

export { FileViewerDialog } from "./FileViewerDialog";
export type { FileViewerDialogProps } from "./FileViewerDialog";
export { resolveKind } from "./registry";
export type {
  FileHandle,
  FileKind,
  FileSource,
  OpenFile,
  ViewBodyProps,
  ViewerActionId,
  ViewerDefinition,
  ViewMode,
} from "./types";
