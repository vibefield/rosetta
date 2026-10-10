// A host's menu (`RoomMenu`), drawn as the room's popover rows from the
// control that opens it: a head's act (R5), ③'s folder (R6), a list's chip
// (R7). The rows are the host's; their faces — the check, the
// mark, the second line, the group heads, the filter past a count — are the
// room's.
//
// The rows are drawn ABOVE the columns, in the room's root (`MenuLayer`):
// every column clips what overflows it, so rows hung inside one lost whichever
// edge crossed it. Anchored to their control, they flip above it when there
// is more room there, and they are clamped inside the room's box.
//
// While it is open it listens for a press outside, for Escape, and for the
// window's resize and scroll (to stay on its control); the arrow keys walk its
// rows. It binds nothing while it is closed. Opened by a key, the chosen row
// takes focus and wears the ring; opened by a pointer, the rows take focus
// without a ring until a key moves in them.
import { createContext, useContext, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { createPortal } from "react-dom";
import type { RoomMenu, RoomMenuItem } from "../host-inputs";

const CHECK = (
  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="m3.5 8.5 3 3 6-7" />
  </svg>
);

/** The room's root, where its menus are drawn over every column. Absent (a
 * part mounted outside a room), a menu hangs from its control as before. */
const MenuLayerContext = createContext<HTMLElement | null>(null);
export const MenuLayerProvider = MenuLayerContext.Provider;

/** The edge the rows keep from the room's box, and from their control. */
const MENU_MARGIN = 8;
const MENU_GAP = 6;
/** The rows' widest, wherever they open: a path past it ellipsizes. */
const MENU_MAX_WIDTH = 360;

export interface MenuBox {
  readonly left: number;
  readonly top: number;
  readonly right: number;
  readonly bottom: number;
}

export interface MenuPlacement {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  /** The most height the rows may take on their side. */
  readonly room: number;
  readonly side: "below" | "above";
}

/**
 * Where the rows go, in the box's own coordinates: under their control
 * (or over it, when there is more room there and not enough below), from the
 * control's start or end edge, and never past the box's edges less a margin.
 */
export function placeMenu(
  control: MenuBox,
  box: MenuBox,
  menu: { readonly width: number; readonly height: number },
  align: "start" | "end",
): MenuPlacement {
  const boxWidth = box.right - box.left;
  const boxHeight = box.bottom - box.top;
  const width = Math.max(0, Math.min(menu.width, boxWidth - 2 * MENU_MARGIN));
  const start = control.left - box.left;
  const end = control.right - box.left;
  const wanted = align === "end" ? end - width : start;
  const left = Math.max(MENU_MARGIN, Math.min(boxWidth - MENU_MARGIN - width, wanted));
  const below = boxHeight - MENU_MARGIN - (control.bottom - box.top + MENU_GAP);
  const above = control.top - box.top - MENU_GAP - MENU_MARGIN;
  const side = menu.height <= below || below >= above ? "below" : "above";
  const room = Math.max(0, side === "below" ? below : above);
  const height = Math.min(menu.height, room);
  const top = side === "below" ? control.bottom - box.top + MENU_GAP : control.top - box.top - MENU_GAP - height;
  return { left, top, width, room, side };
}

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
  const [open, setOpen] = useState<"pointer" | "key" | null>(null);
  const rootRef = useRef<HTMLSpanElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  // A press that starts with a pointer opens without a ring; a key does not
  // send a pointer first.
  const pointerPress = useRef(false);
  const layer = useContext(MenuLayerContext);
  const id = useId();

  useEffect(() => {
    if (open === null) return;
    const pointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (rootRef.current?.contains(target) || menuRef.current?.contains(target)) return;
      setOpen(null);
    };
    const keyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setOpen(null);
      triggerRef.current?.focus();
    };
    document.addEventListener("pointerdown", pointerDown);
    document.addEventListener("keydown", keyDown);
    return () => {
      document.removeEventListener("pointerdown", pointerDown);
      document.removeEventListener("keydown", keyDown);
    };
  }, [open]);

  const rows = open === null ? null : (
    <HostMenuRows
      id={id}
      menu={menu}
      align={align}
      opened={open}
      control={triggerRef}
      layer={layer}
      menuRef={menuRef}
      onChosen={() => {
        setOpen(null);
        triggerRef.current?.focus();
      }}
    />
  );

  return (
    <span className="ah-menuhost" ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className={className}
        title={title}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open !== null}
        aria-controls={open !== null ? id : undefined}
        disabled={disabled === true}
        {...data}
        onPointerDown={() => {
          pointerPress.current = true;
        }}
        onClick={() => {
          const by = pointerPress.current ? "pointer" : "key";
          pointerPress.current = false;
          setOpen((current) => (current === null ? by : null));
        }}
      >
        {children}
      </button>
      {rows !== null && (layer === null ? rows : createPortal(rows, layer))}
    </span>
  );
}

function matches(item: RoomMenuItem, term: string): boolean {
  return item.filterText === undefined || item.filterText.toLowerCase().includes(term);
}

function rectOf(element: Element): MenuBox {
  const rect = element.getBoundingClientRect();
  return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
}

function HostMenuRows({
  id,
  menu,
  align,
  opened,
  control,
  layer,
  menuRef,
  onChosen,
}: {
  readonly id: string;
  readonly menu: RoomMenu;
  readonly align: "start" | "end";
  readonly opened: "pointer" | "key";
  readonly control: { readonly current: HTMLElement | null };
  readonly layer: HTMLElement | null;
  readonly menuRef: { current: HTMLDivElement | null };
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

  // Placed in the room's box, before the first paint and whenever the window
  // moves under it. Measured at its natural size, then held to the room it has.
  useLayoutEffect(() => {
    const element = menuRef.current;
    if (layer === null || element === null) return;
    const place = () => {
      const anchor = control.current;
      if (anchor === null) return;
      const box = layer.getBoundingClientRect();
      // a box drawn scaled (a host's recede) is measured in its own pixels
      const scale = layer.offsetWidth > 0 ? box.width / layer.offsetWidth : 1;
      const within = (rect: MenuBox): MenuBox => ({
        left: (rect.left - box.left) / scale,
        top: (rect.top - box.top) / scale,
        right: (rect.right - box.left) / scale,
        bottom: (rect.bottom - box.top) / scale,
      });
      const frame = within(rectOf(layer));
      element.style.maxHeight = "";
      element.style.maxWidth = `${Math.max(0, Math.min(MENU_MAX_WIDTH, frame.right - frame.left - 2 * MENU_MARGIN))}px`;
      const placed = placeMenu(within(rectOf(anchor)), frame, { width: element.offsetWidth, height: element.offsetHeight }, align);
      element.style.left = `${placed.left}px`;
      element.style.top = `${placed.top}px`;
      element.style.maxHeight = `${placed.room}px`;
      element.dataset.side = placed.side;
    };
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [layer, control, menuRef, align, groups.length, filtering]);

  // Opening lands on the filter; else, by a key, on the chosen row (which
  // wears the ring), and by a pointer on the rows themselves (no ring).
  useLayoutEffect(() => {
    if (filtering) {
      filterRef.current?.focus();
      return;
    }
    if (opened === "pointer") {
      listRef.current?.focus();
      return;
    }
    const all = rows();
    (all.find((row) => row.getAttribute("aria-checked") === "true") ?? all[0])?.focus();
    // once, on open
  }, []);

  const walk = (event: KeyboardEvent<HTMLDivElement>) => {
    const target = event.target as HTMLElement;
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const all = rows();
    if (all.length === 0) return;
    event.preventDefault();
    const at = all.indexOf(target as HTMLButtonElement);
    const next =
      event.key === "Home" ? 0
      : event.key === "End" ? all.length - 1
      : event.key === "ArrowDown" ? (at + 1) % all.length
      : at <= 0 ? all.length - 1 : at - 1;
    all[next]?.focus();
  };

  const item = (row: RoomMenuItem) => {
    const choice = (
      <button
        key={row.key}
        type="button"
        className="ah-menu__item"
        role={radio ? "menuitemradio" : "menuitem"}
        aria-checked={radio ? row.checked === true : undefined}
        data-depth={row.depth === 1 ? "1" : undefined}
        data-key={row.key}
        disabled={row.disabled === true}
        tabIndex={-1}
        onClick={() => {
          row.onSelect?.();
          onChosen();
        }}
      >
        <span className="ah-menu__check">{row.checked === true ? CHECK : null}</span>
        {row.mark !== undefined && <span className="ah-menu__mark">{row.mark}</span>}
        <span className="ah-menu__body">
          <span className="ah-menu__label">{row.label}</span>
          {row.detail !== undefined && <span className="ah-menu__detail">{row.detail}</span>}
        </span>
        {row.aside !== undefined && <span className="ah-menu__aside">{row.aside}</span>}
      </button>
    );
    return choice;
  };

  return (
    <div
      className="ah-menu"
      ref={menuRef}
      data-align={align}
      data-layer={layer === null ? undefined : ""}
      onKeyDown={walk}
    >
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
      <div className="ah-menu__list" id={id} role="menu" aria-label={menu.label} ref={listRef} tabIndex={-1}>
        {groups.map((group) => (
          <div className="ah-menu__group" role="group" key={group.key}>
            {group.title !== undefined && <div className="ah-menu__title">{group.title}</div>}
            {group.items.map(item)}
          </div>
        ))}
        {groups.length === 0 && <p className="ah-menu__empty">{menu.empty ?? "Nothing to choose"}</p>}
      </div>
    </div>
  );
}
