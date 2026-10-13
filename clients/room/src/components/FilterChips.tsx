// A host's filter chips (R7): each a button in the list's bar, in the room's
// own chip pill, that opens its rows. The host filters — through the list's
// sections — and says what is chosen with the chip's words, its set look and
// a checked row; the room only draws and opens them.
import type { RoomFilter } from "../host-inputs";
import { HostMenuButton } from "./HostMenu";

export function FilterChips({ filters }: { readonly filters: readonly RoomFilter[] }) {
  return (
    <span className="ah-chips">
      {filters.map((filter) => (
        <HostMenuButton
          key={filter.key}
          menu={filter.menu}
          className="ah-chipselect ah-chip"
          label={filter.accessibleLabel}
          align="start"
          data={{ "data-filter": filter.key, "data-set": filter.set === true ? "" : undefined }}
        >
          <span className="ah-chip__label">{filter.label}</span>
        </HostMenuButton>
      ))}
    </span>
  );
}
