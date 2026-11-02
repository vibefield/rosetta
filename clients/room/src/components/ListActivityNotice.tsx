import { arrivalsLabel } from "../lib/import-state";
import type { ImportArrivals } from "../lib/import-state";

export function ListActivityNotice({ pending, onShow, noun, arrivals = null }: {
  readonly pending: boolean;
  readonly onShow: () => void;
  readonly noun: "projects" | "sessions";
  /** Conversations the import opened while the list was held: the pill
   * counts them, and says "older" only when every one lands below the
   * reader's view. */
  readonly arrivals?: ImportArrivals | null;
}) {
  const imported = pending && arrivals !== null ? arrivalsLabel(arrivals) : null;
  return (
    <>
      {/* The status announces what kind of change waits, never its count:
          the count moves with every tree the import opens, and a polite
          region would announce each one. The button carries the figure. */}
      <span className="ah-list-update-status" role="status">
        {!pending ? "" : imported !== null
          ? `Newly imported conversations are available in ${noun}.`
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
        {imported !== null ? `${imported} — show` : "Show recent activity"}
      </button>
    </>
  );
}
