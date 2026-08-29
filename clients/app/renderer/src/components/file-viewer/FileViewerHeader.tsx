// The dialog's band. Same anatomy as every column head in the room — tint
// marker, name, and one fact travelling beside the name — because a dialog
// showing one object is doing the same job a column showing one object does,
// and the room should not have two ways of naming a thing.
//
// The name is a file name and the second line is a path, so both set in mono:
// the design record's rule that an id is mono, applied one level down.

import { ColumnHeading } from "../ui";
import { SegmentedControl } from "../ui";
import { CloseGlyph, CopyGlyph, ExternalGlyph } from "../../lib/icons";
import { fmtBytes, tildePath } from "../../lib/format";
import type { FileHandle, FileKind, ViewMode } from "./types";

export interface HeaderAction {
  readonly id: string;
  readonly label: string;
  readonly glyph: "copy" | "external";
  readonly run: () => void;
}

export interface FileViewerHeaderProps {
  readonly handle: FileHandle;
  /** What the registry decided, or `null` before the read resolves it — the
   * band names the file either way. */
  readonly kind: FileKind | null;
  readonly tint: string | null;
  readonly homeDirectory: string | null;
  readonly modes: readonly ViewMode[];
  readonly mode: string;
  readonly onModeChange: (mode: string) => void;
  readonly actions: readonly HeaderAction[];
  readonly onClose: () => void;
}

/** The folder the file is in, tilde-abbreviated. `null` when the caller could
 * not derive a path, which is a missing line rather than a wrong one. */
function directoryOf(path: string | null, home: string | null): string | null {
  if (path === null) return null;
  const cut = path.lastIndexOf("/");
  return cut <= 0 ? path : tildePath(path.slice(0, cut), home);
}

export function FileViewerHeader({
  handle,
  kind,
  tint,
  homeDirectory,
  modes,
  mode,
  onModeChange,
  actions,
  onClose,
}: FileViewerHeaderProps) {
  const directory = directoryOf(handle.path, homeDirectory);

  return (
    <header className="ah-fileview__head">
      <div className="ah-fileview__title">
        <ColumnHeading name={handle.name} tint={tint} mono title={handle.path} />
        {kind !== null && <span className="ah-fileview__kind">{kind.label}</span>}
        <span className="ah-caption tnum">{fmtBytes(handle.size)}</span>
      </div>

      <div className="ah-fileview__acts">
        {modes.length > 1 && (
          <SegmentedControl
            className="ah-tabseg"
            accessibleLabel="How to read this file"
            value={mode}
            options={modes.map((option) => ({ value: option.id, label: option.label }))}
            onValueChange={onModeChange}
          />
        )}
        {actions.map((action) => (
          <button
            className="ah-iconbtn"
            key={action.id}
            type="button"
            title={action.label}
            aria-label={action.label}
            onClick={action.run}
          >
            {action.glyph === "copy" ? <CopyGlyph /> : <ExternalGlyph />}
          </button>
        ))}
        <button
          className="ah-iconbtn"
          type="button"
          title="Close (Esc)"
          aria-label="Close"
          onClick={onClose}
        >
          <CloseGlyph />
        </button>
      </div>

      {directory !== null && (
        <p className="ah-fileview__path ah-idname" title={handle.path ?? undefined}>
          <bdi>{directory}</bdi>
        </p>
      )}
    </header>
  );
}
