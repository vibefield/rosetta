import type { CSSProperties, ReactNode } from "react";

type ResultGroupHeaderStyle = CSSProperties & { "--tint": string };

export interface ResultGroupHeaderProps {
  readonly label: ReactNode;
  readonly count: number;
  readonly tint: string;
  readonly countLabel?: string;
}

/** Header for one grouped set of search results. */
export function ResultGroupHeader({
  label,
  count,
  tint,
  countLabel,
}: ResultGroupHeaderProps) {
  return (
    <div className="ah-group" style={{ "--tint": tint } as ResultGroupHeaderStyle}>
      <i aria-hidden="true" />
      {label}
      <span
        className="ah-hitcount"
        aria-label={countLabel ?? `${count} ${count === 1 ? "result" : "results"}`}
      >
        {count}
      </span>
    </div>
  );
}
