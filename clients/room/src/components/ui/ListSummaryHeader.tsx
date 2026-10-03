import type { ReactNode } from "react";

export interface ListSummaryHeaderProps {
  readonly label: ReactNode;
  readonly count: number;
  readonly singularNoun: string;
  readonly pluralNoun?: string;
}

/** Compact replacement for an expanded summary panel while a list is filtered. */
export function ListSummaryHeader({
  label,
  count,
  singularNoun,
  pluralNoun = `${singularNoun}s`,
}: ListSummaryHeaderProps) {
  const noun = count === 1 ? singularNoun : pluralNoun;

  return (
    <div className="ah-meta">
      <div className="ah-meta__head">
        <span className="ah-eyebrow">{label}</span>
        <span className="ah-caption tnum">
          {count} {noun}
        </span>
      </div>
    </div>
  );
}
