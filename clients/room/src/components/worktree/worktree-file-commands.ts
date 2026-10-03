import type { EntryId, FileExplorer } from "@vibecook/mille";
import { fileActionTargetForId } from "@vibecook/mille-ui";
import { createCommandRegistry } from "@vibecook/mille-ui/commands";
import type { CommandContext } from "@vibecook/mille-ui/commands";
import type { WorktreeFileActionRequest, WorktreeHostPort } from "../../ports";
import type { WorktreeEngine } from "./types";

interface FileActionEnvironment {
  /** The host's worktree engine, which performs the acts; null without one. */
  readonly actions: Pick<WorktreeHostPort, "fileAction"> | null;
  /** The desktop's platform, for the name of its file manager. */
  readonly platform: string | null;
  readonly stale: boolean;
  readonly onReceipt: (message: string) => void;
}

/** One registry per engine generation. Menu commands receive Mille's live
 * context; keyboard commands get the same selection through its observers. */
export function createWorktreeFileCommands(
  engine: WorktreeEngine,
  environment: () => FileActionEnvironment,
) {
  let selectedIds: ReadonlySet<EntryId> = new Set();
  let focusedId: EntryId | null = null;
  const emptyIds: ReadonlySet<EntryId> = new Set();
  const available = () => {
    const { actions, stale } = environment();
    return !stale && typeof actions?.fileAction === "function";
  };
  const hasSelection = (ctx: CommandContext) => available() && ctx.selectedIds.size > 0;

  async function perform(ctx: CommandContext, action: WorktreeFileActionRequest["action"]) {
    const { actions, platform, onReceipt } = environment();
    if (!available() || actions === null || ctx.selectedIds.size === 0) return;
    const ids = action === "copyPath"
      ? [...ctx.selectedIds]
      : ctx.focusedId !== null && ctx.selectedIds.has(ctx.focusedId)
        ? [ctx.focusedId]
        : [];
    if (ids.length === 0) return;
    try {
      const paths = ids.map((id) => {
        const target = fileActionTargetForId(ctx.snapshot, id);
        if (target === null) throw new Error("That file is no longer in the tree. Select it again.");
        return target.rootRelativePath;
      }).sort();
      const result = await actions.fileAction({
        root: engine.root,
        generation: engine.generation,
        action,
        paths,
      });
      if (!result.ok) throw new Error(result.error);
      if (action === "copyPath") {
        onReceipt(paths.length === 1 ? "Path copied" : `${paths.length} paths copied`);
      } else {
        onReceipt(platform === "darwin" ? "Revealed in Finder" : "Revealed in file manager");
      }
    } catch (error) {
      onReceipt(error instanceof Error ? error.message : "Could not complete the file action.");
    }
  }

  const commands = createCommandRegistry([
    {
      id: "file.copyAbsolutePath",
      label: "Copy Path",
      keybinding: ["Mod+C", "Ctrl+C", "Cmd+C"],
      group: "path",
      order: 0,
      enablement: hasSelection,
      run: (ctx) => perform(ctx, "copyPath"),
    },
    {
      id: "file.revealInFileManager",
      label: () => environment().platform === "darwin"
        ? "Open in Finder"
        : "Open in File Manager",
      group: "path",
      order: 1,
      enablement: (ctx) =>
        hasSelection(ctx) && ctx.focusedId !== null && ctx.selectedIds.has(ctx.focusedId),
      run: (ctx) => perform(ctx, "revealInFileManager"),
    },
    // Mille installs its mutating Paste command if this id is absent. Reserve
    // it explicitly so it stays unavailable in menus and keyboard dispatch.
    {
      id: "file.paste",
      label: "Paste",
      visibleInContextMenu: false,
      enablement: () => false,
      run: () => {},
    },
  ]);
  commands.setContextProvider(() => {
    // Mille types its registry against the in-process engine; our port exposes
    // the same snapshot methods used by these commands, as it does for the tree.
    const fx = engine.fx as unknown as FileExplorer;
    const snapshot = fx.getSnapshot();
    return {
      fx,
      snapshot,
      focusedId,
      focusedEntry: focusedId === null ? null : snapshot.getById(focusedId),
      selectedIds,
      selectedEntries: [...selectedIds].flatMap((id) => {
        const entry = snapshot.getById(id);
        return entry === null ? [] : [entry];
      }),
      isMultiSelect: selectedIds.size > 1,
      isRenaming: false,
      host: {},
      cutIds: emptyIds,
      copyIds: emptyIds,
    };
  });
  return {
    commands,
    onSelectionChange: (ids: ReadonlySet<EntryId>) => {
      selectedIds = ids;
    },
    onFocusedIdChange: (id: EntryId | null) => {
      focusedId = id;
    },
  };
}
