import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { FocusEvent, PointerEvent, UIEvent } from "react";

export interface ListPosition {
  readonly id: string;
  readonly day: number | null;
}

interface Interaction {
  scope: string;
  hovered: boolean;
  focused: boolean;
  pressed: boolean;
  away: boolean;
  scrolling: boolean;
}

const idle = (scope: string): Interaction => ({
  scope, hovered: false, focused: false, pressed: false, away: false, scrolling: false,
});
const signature = (items: readonly ListPosition[]) => JSON.stringify(items);

/** Hold geometry, never row data. Pointer, keyboard and scroll activity all
 * protect the same snapshot, including day membership and new insertions. */
export function useDeferredListOrder(desired: readonly ListPosition[], scope: string) {
  const wanted = signature(desired);
  const [view, setView] = useState({ scope, items: desired, signature: wanted });
  const [interaction, setInteraction] = useState(() => idle(scope));
  const scrollRef = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const reveal = useRef(false);
  const focusAfterReveal = useRef(false);
  const anchor = useRef<{ scope: string; rows: { id: string; offset: number }[] } | null>(null);
  const held = interaction.scope === scope && (
    interaction.hovered || interaction.focused || interaction.pressed ||
    interaction.away || interaction.scrolling
  );
  let current = view;
  if (view.scope !== scope || (!held && view.signature !== wanted)) {
    current = { scope, items: desired, signature: wanted };
    setView(current);
    if (interaction.scope !== scope) setInteraction(idle(scope));
  } else if (held && view.signature !== wanted) {
    // Authoritative removals cannot leave clickable ghosts behind. Preserve
    // the first surviving visible row when removing something above it.
    const present = new Set(desired.map(item => item.id));
    const remaining = view.items.filter(item => present.has(item.id));
    if (remaining.length !== view.items.length) {
      current = { scope, items: remaining, signature: signature(remaining) };
      setView(current);
    }
  }

  const mark = (patch: Partial<Interaction>) => setInteraction(previous => {
    const base = previous.scope === scope ? previous : idle(scope);
    return base === previous && (Object.keys(patch) as (keyof Interaction)[]).every(key => base[key] === patch[key])
      ? previous : { ...base, ...patch };
  });
  const rememberAnchor = () => {
    const container = scrollRef.current;
    if (!container || container.scrollTop <= 2) {
      anchor.current = null;
      return;
    }
    const elements = container.querySelectorAll<HTMLElement>("[data-motion-key]");
    let low = 0;
    let high = elements.length;
    while (low < high) {
      const mid = (low + high) >>> 1;
      const row = elements[mid];
      if (row.offsetTop + row.offsetHeight <= container.scrollTop) low = mid + 1;
      else high = mid;
    }
    const rows: { id: string; offset: number }[] = [];
    for (let index = low; index < Math.min(low + 3, elements.length); index++) {
      const row = elements[index];
      rows.push({ id: row.dataset.motionKey!, offset: row.offsetTop - container.scrollTop });
    }
    anchor.current = { scope, rows };
  };
  useLayoutEffect(() => {
    const container = scrollRef.current;
    if (!container) return;
    if (reveal.current) {
      reveal.current = false;
      container.scrollTop = 0;
      if (focusAfterReveal.current) {
        container.querySelector<HTMLElement>("[data-motion-key]")?.focus({ preventScroll: true });
      }
      focusAfterReveal.current = false;
    } else if (anchor.current?.scope === scope) {
      const rows = new Map(Array.from(container.querySelectorAll<HTMLElement>("[data-motion-key]"),
        row => [row.dataset.motionKey!, row]));
      for (const saved of anchor.current.rows) {
        const row = rows.get(saved.id);
        if (!row) continue;
        container.scrollTop = row.offsetTop - saved.offset;
        break;
      }
    }
    rememberAnchor();
  });
  useLayoutEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
    anchor.current = null;
  }, [scope]);
  useEffect(() => () => {
    if (timer.current !== null) clearTimeout(timer.current);
  }, [scope]);

  const scrollActivity = () => {
    mark({ scrolling: true });
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      setInteraction(previous => previous.scope === scope ? { ...previous, scrolling: false } : previous);
      timer.current = null;
    }, 180);
  };

  return {
    items: current.items,
    pending: current.signature !== wanted,
    scrollRef,
    hostProps: {
      onPointerEnter: (event: PointerEvent<HTMLDivElement>) => {
        if (event.pointerType !== "touch") mark({ hovered: true });
      },
      onPointerLeave: () => mark({ hovered: false, pressed: false }),
      onPointerDownCapture: () => mark({ pressed: true }),
      onPointerUpCapture: () => mark({ pressed: false }),
      onPointerCancel: () => mark({ pressed: false }),
      onFocusCapture: () => mark({ focused: true }),
      onBlurCapture: (event: FocusEvent<HTMLDivElement>) => {
        if (!event.currentTarget.contains(event.relatedTarget)) mark({ focused: false });
      },
      onWheelCapture: scrollActivity,
    },
    onScroll: (event: UIEvent<HTMLDivElement>) => {
      mark({ away: event.currentTarget.scrollTop > 2 });
      rememberAnchor();
      scrollActivity();
    },
    apply: () => {
      focusAfterReveal.current = document.activeElement?.hasAttribute("data-list-updates") ?? false;
      reveal.current = true;
      setView({ scope, items: desired, signature: wanted });
      mark({ away: false });
    },
  };
}
