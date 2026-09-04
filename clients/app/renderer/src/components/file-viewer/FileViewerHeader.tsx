// The dialog's band. Same anatomy as every column head in the room — tint
// marker, name, and one fact travelling beside the name — because a dialog
// showing one object is doing the same job a column showing one object does,
// and the room should not have two ways of naming a thing.
//
// The name is a file name and the second line is a path, so both set in mono:
// the design record's rule that an id is mono, applied one level down.

import { ColumnHeading } from "../ui";
import { SegmentedControl } from "../ui";
import {
  CloseGlyph,
  CopyGlyph,
  ExitFullscreenGlyph,
  ExternalGlyph,
  FullscreenGlyph,
} from "../../lib/icons";
import { fmtBytes, tildePath } from "../../lib/format";
import type { FileHandle, FileKind, ViewMode } from "./types";

export interface HeaderAction {
  readonly id: string;
  readonly label: string;
  readonly glyph: "copy" | "external";
  readonly disabled?: boolean;
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
  readonly fullscreen: boolean;
  readonly onToggleFullscreen: () => void;
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
  fullscreen,
  onToggleFullscreen,
  onClose,
}: FileViewerHeaderProps) {
  const directory = directoryOf(handle.path, homeDirectory);

  return (
    <header className="ah-fileview__head">
      <div className="ah-fileview__title">
        {/* `null` would mean "no tooltip at all" — and a file with no
            derivable path is exactly when a truncated name needs one. */}
        <ColumnHeading name={handle.name} tint={tint} mono title={handle.path ?? undefined} />
        {kind !== null && <span className="ah-fileview__kind">{kind.label}</span>}
        {/* A change has no size of its own, and `0 B` would be a figure the
            room invented. Silence is the honest slot — the diff's own bar
            carries `+2 −1`, which is the fact a reader wanted anyway. */}
        {handle.size > 0 && <span className="ah-caption tnum">{fmtBytes(handle.size)}</span>}
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
            disabled={action.disabled}
            onClick={action.run}
          >
            {action.glyph === "copy" ? <CopyGlyph /> : <ExternalGlyph />}
          </button>
        ))}
        <button
          className="ah-iconbtn"
          type="button"
          title={fullscreen ? "Exit full screen (Esc)" : "View full screen"}
          aria-label={fullscreen ? "Exit full screen" : "View full screen"}
          aria-pressed={fullscreen}
          onClick={onToggleFullscreen}
        >
          {fullscreen ? <ExitFullscreenGlyph /> : <FullscreenGlyph />}
        </button>
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
