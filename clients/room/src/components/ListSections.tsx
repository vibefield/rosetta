// A host's sections, drawn (R3): a head over each run, in the list's own day
// face, and a foot section folded into one line that opens in place. The
// rows themselves are the list's; this draws only what sits between them.
import { useCallback, useEffect, useState } from "react";
import type { ReactNode } from "react";
import type { ArrangedSection } from "../lib/sections";
import { foldCount } from "../lib/sections";

/** A section's head: the list's own break, in the host's words. */
export function SectionHead({ sectionKey, children }: { readonly sectionKey: string; readonly children: ReactNode }) {
  return (
    <div className="ah-day ah-section" data-section={sectionKey}>
      {children}
    </div>
  );
}

/** A folded section's line: the host's words, the count, and the act that
 * opens it. Open, the line stays as the section's head and closes it again. */
export function FoldLine<T>({
  arranged,
  open,
  onToggle,
}: {
  readonly arranged: ArrangedSection<T>;
  readonly open: boolean;
  readonly onToggle: () => void;
}) {
  const fold = arranged.section.fold!;
  return (
    <button
      type="button"
      className="ah-fold"
      data-section={arranged.section.key}
      aria-expanded={open}
      onClick={onToggle}
    >
      <span className="ah-fold__label">{fold.label}</span>
      <span className="ah-fold__count tnum">{foldCount(arranged)}</span>
      {!open && <span className="ah-fold__act">{fold.act}</span>}
    </button>
  );
}

/** Which folded sections are open. Each starts folded; one holding the
 * selected row opens, and after that the reader's press decides. */
export function useOpenFolds<T>(
  arranged: readonly ArrangedSection<T>[] | null,
  idOf: (row: T) => string,
  selectedId: string | null,
): { readonly isOpen: (key: string) => boolean; readonly toggle: (key: string) => void } {
  const [opened, setOpened] = useState<ReadonlySet<string>>(() => new Set());
  const holding =
    selectedId === null || arranged === null
      ? null
      : (arranged.find((entry) => entry.section.fold !== undefined && entry.rows.some((row) => idOf(row) === selectedId))
          ?.section.key ?? null);
  useEffect(() => {
    if (holding === null) return;
    setOpened((current) => (current.has(holding) ? current : new Set([...current, holding])));
  }, [holding, selectedId]);
  const toggle = useCallback((key: string) => {
    setOpened((current) => {
      const next = new Set(current);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  }, []);
  return { isOpen: (key) => opened.has(key), toggle };
}
