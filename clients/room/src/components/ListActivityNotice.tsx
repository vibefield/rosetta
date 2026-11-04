import { arrivalsLabel } from "../lib/import-state";
import type { ImportArrivals } from "../lib/import-state";

export function ListActivityNotice({ pending, onShow, noun, arrivals = null, importReorder = false }: {
  readonly pending: boolean;
  readonly onShow: () => void;
  readonly noun: "projects" | "sessions";
  /** Conversations the import opened while the list was held: the pill
   * counts them, and says "older" only when every one lands below the
   * reader's view. */
  readonly arrivals?: ImportArrivals | null;
  /** The waiting order comes from the import opening conversations, not
   * from new activity: the pill names the import as its cause. */
  readonly importReorder?: boolean;
}) {
  const imported = pending && arrivals !== null ? arrivalsLabel(arrivals) : null;
  const byImport = pending && imported === null && importReorder;
  const Noun = `${noun.charAt(0).toUpperCase()}${noun.slice(1)}`;
  return (
    <>
      {/* The status announces what kind of change waits, never its count:
          the count moves with every tree the import opens, and a polite
          region would announce each one. The button carries the figure. */}
      <span className="ah-list-update-status" role="status">
        {!pending ? "" : imported !== null
          ? `Newly imported conversations are available in ${noun}.`
          : byImport ? `${Noun} were reordered by the import.`
          : `New activity is available in ${noun}.`}
      </span>
      <button
        type="button"
        className="ah-newpill ah-list-updates"
        data-list-updates=""
        data-visible={pending ? "" : undefined}
        hidden={!pending}
        onClick={onShow}
      >
        {imported !== null ? `${imported} — show` : byImport ? `${Noun} reordered by the import · show` : "Show recent activity"}
      </button>
    </>
  );
}
