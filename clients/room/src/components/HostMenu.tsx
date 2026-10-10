// A host's menu (`RoomMenu`), drawn as the room's popover rows under the
// control that opens it: a head's act (R5), ③'s folder (R6), a list's chip
// (R7). The rows are the host's; their faces — the check, the mark, the
// second line, the group heads, the filter past a count — are the room's.
// While it is open it listens for a press outside and for Escape, and the
// arrow keys walk its rows; it binds nothing while it is closed.
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import type { RoomMenu, RoomMenuItem } from "../host-inputs";

const CHECK = (
  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="m3.5 8.5 3 3 6-7" />
  </svg>
);

export interface HostMenuButtonProps {
  readonly menu: RoomMenu;
  /** The control's own class and face; the room's look for its slot. */
  readonly className: string;
  readonly label: string;
  readonly title?: string | undefined;
  readonly disabled?: boolean | undefined;
  /** Data attributes the slot marks its control with. */
  readonly data?: Readonly<Record<`data-${string}`, string | undefined>> | undefined;
  /** Which edge of the control the rows hang from. */
  readonly align?: "start" | "end" | undefined;
  readonly children: ReactNode;
}

/** A control that opens the host's rows. */
export function HostMenuButton({ menu, className, label, title, disabled, data, align = "start", children }: HostMenuButtonProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLSpanElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const id = useId();

  useEffect(() => {
    if (!open) return;
    const pointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const keyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setOpen(false);
      triggerRef.current?.focus();
    };
    document.addEventListener("pointerdown", pointerDown);
    document.addEventListener("keydown", keyDown);
    return () => {
      document.removeEventListener("pointerdown", pointerDown);
      document.removeEventListener("keydown", keyDown);
    };
  }, [open]);

  return (
    <span className="ah-menuhost" ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className={className}
        title={title}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        disabled={disabled === true}
        {...data}
        onClick={() => setOpen((current) => !current)}
      >
        {children}
      </button>
      {open && (
        <HostMenuRows
          id={id}
          menu={menu}
          align={align}
          onChosen={() => {
            setOpen(false);
            triggerRef.current?.focus();
          }}
        />
      )}
    </span>
  );
}

function matches(item: RoomMenuItem, term: string): boolean {
  return item.filterText === undefined || item.filterText.toLowerCase().includes(term);
}

function HostMenuRows({
  id,
  menu,
  align,
  onChosen,
}: {
  readonly id: string;
  readonly menu: RoomMenu;
  readonly align: "start" | "end";
  readonly onChosen: () => void;
}) {
  const [filter, setFilter] = useState("");
  const listRef = useRef<HTMLDivElement | null>(null);
  const filterRef = useRef<HTMLInputElement | null>(null);
  const total = menu.groups.reduce((count, group) => count + group.items.length, 0);
  const filtering = menu.filterAfter !== undefined && total > menu.filterAfter;
  const term = filter.trim().toLowerCase();
  const groups = !filtering || term === ""
    ? menu.groups
    : menu.groups
        .map((group) => ({ ...group, items: group.items.filter((item) => matches(item, term)) }))
        .filter((group) => group.items.length > 0);
  const radio = menu.groups.some((group) => group.items.some((item) => item.checked !== undefined));

  const rows = () => [...(listRef.current?.querySelectorAll<HTMLButtonElement>(".ah-menu__item:not(:disabled)") ?? [])];
  // Opening lands on the filter, else on the chosen row, else the first.
  useLayoutEffect(() => {
    if (filtering) {
      filterRef.current?.focus();
      return;
    }
    const all = rows();
    (all.find((row) => row.getAttribute("aria-checked") === "true") ?? all[0])?.focus();
    // once, on open
  }, []);

  const walk = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const all = rows();
    if (all.length === 0) return;
    event.preventDefault();
    const at = all.indexOf(document.activeElement as HTMLButtonElement);
    const next =
      event.key === "Home" ? 0
      : event.key === "End" ? all.length - 1
      : event.key === "ArrowDown" ? (at + 1) % all.length
      : at <= 0 ? all.length - 1 : at - 1;
    all[next]?.focus();
  };

  return (
    <div className="ah-menu" data-align={align} onKeyDown={walk}>
      {filtering && (
        <input
          ref={filterRef}
          className="ah-menu__filter"
          type="search"
          value={filter}
          placeholder={menu.filterPlaceholder ?? "Filter"}
          aria-label={menu.filterPlaceholder ?? "Filter"}
          aria-controls={id}
          onChange={(event) => setFilter(event.currentTarget.value)}
        />
      )}
      <div className="ah-menu__list" id={id} role="menu" aria-label={menu.label} ref={listRef}>
        {groups.map((group) => (
          <div className="ah-menu__group" role="group" key={group.key}>
            {group.title !== undefined && <div className="ah-menu__title">{group.title}</div>}
            {group.items.map((item) => (
              <button
                key={item.key}
                type="button"
                className="ah-menu__item"
                role={radio ? "menuitemradio" : "menuitem"}
                aria-checked={radio ? item.checked === true : undefined}
                data-depth={item.depth === 1 ? "1" : undefined}
                data-key={item.key}
                disabled={item.disabled === true}
                tabIndex={-1}
                onClick={() => {
                  item.onSelect?.();
                  onChosen();
                }}
              >
                <span className="ah-menu__check">{item.checked === true ? CHECK : null}</span>
                {item.mark !== undefined && <span className="ah-menu__mark">{item.mark}</span>}
                <span className="ah-menu__body">
                  <span className="ah-menu__label">{item.label}</span>
                  {item.detail !== undefined && <span className="ah-menu__detail">{item.detail}</span>}
                </span>
                {item.aside !== undefined && <span className="ah-menu__aside">{item.aside}</span>}
              </button>
            ))}
          </div>
        ))}
        {groups.length === 0 && <p className="ah-menu__empty">{menu.empty ?? "Nothing to choose"}</p>}
      </div>
    </div>
  );
}
